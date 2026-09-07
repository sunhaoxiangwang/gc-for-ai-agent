//! Tests for the invariants in the safety model that are properties of the
//! build rather than of any single function.
//!
//! Each test names the invariant it covers.

use std::path::Path;

/// Invariant 3: the planner cannot delete.
///
/// `reap-core` is where every decision about what to reclaim is made. If it
/// cannot express a mutation, then no bug in the planner or the guard stack can
/// turn into a removal, whatever else goes wrong. This is checked against the
/// source rather than argued in a comment so it cannot quietly stop being true.
#[test]
fn invariant_3_core_contains_no_filesystem_write_code() {
    // Every std entry point that creates, renames, truncates or removes.
    const FORBIDDEN: &[&str] = &[
        "remove_file",
        "remove_dir",
        "remove_dir_all",
        "fs::rename",
        "fs::write",
        "fs::copy",
        "File::create",
        "OpenOptions",
        "create_dir",
        "create_dir_all",
        "set_permissions",
        "set_times",
        "set_len",
        "hard_link",
        "fs::symlink",
        "os::unix::fs::symlink",
        "std::process::Command",
    ];

    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("crates/reap-core/src");
    let mut findings = Vec::new();

    visit(&src, &mut |path, text| {
        for (i, line) in text.lines().enumerate() {
            let code = strip_comment(line);
            for needle in FORBIDDEN {
                if mentions(code, needle) {
                    findings.push(format!(
                        "{}:{}: {needle} in `{}`",
                        path.display(),
                        i + 1,
                        code.trim()
                    ));
                }
            }
        }
    });

    assert!(
        findings.is_empty(),
        "reap-core must contain no filesystem write code, but found:\n{}",
        findings.join("\n")
    );
}

/// Invariant 3, second half: `reap-core` must not reach for a mutation by way
/// of an unsafe block or a foreign function either.
#[test]
fn invariant_3_core_forbids_unsafe() {
    let lib = Path::new(env!("CARGO_MANIFEST_DIR")).join("crates/reap-core/src/lib.rs");
    let text = std::fs::read_to_string(&lib).expect("read core lib.rs");
    assert!(
        text.contains("#![forbid(unsafe_code)]"),
        "reap-core must declare #![forbid(unsafe_code)]"
    );
}

/// `unsafe` is confined to the platform crate, where the macOS process
/// inspection FFI lives.
#[test]
fn unsafe_appears_only_in_the_platform_crate() {
    let crates = Path::new(env!("CARGO_MANIFEST_DIR")).join("crates");
    let mut findings = Vec::new();
    for crate_name in ["reap-core", "reap-cli"] {
        visit(&crates.join(crate_name).join("src"), &mut |path, text| {
            for (i, line) in text.lines().enumerate() {
                let code = strip_comment(line);
                // `unsafe_code` in a lint attribute is the opposite of a
                // violation.
                if code.contains("unsafe") && !code.contains("unsafe_code") {
                    findings.push(format!("{}:{}: {}", path.display(), i + 1, code.trim()));
                }
            }
        });
    }
    assert!(
        findings.is_empty(),
        "unsafe belongs only in reap-platform, but found:\n{}",
        findings.join("\n")
    );
}

/// Whether `code` names `needle` as a whole identifier.
///
/// Without the boundary check, `fs::symlink` would match the read-only
/// `fs::symlink_metadata`, which the planner uses constantly.
fn mentions(code: &str, needle: &str) -> bool {
    let mut from = 0;
    while let Some(i) = code[from..].find(needle) {
        let end = from + i + needle.len();
        let next_is_ident = code[end..]
            .chars()
            .next()
            .is_some_and(|c| c.is_alphanumeric() || c == '_');
        if !next_is_ident {
            return true;
        }
        from = from + i + needle.len();
    }
    false
}

fn strip_comment(line: &str) -> &str {
    let trimmed = line.trim_start();
    if trimmed.starts_with("//") {
        return "";
    }
    match line.find("//") {
        Some(i) => &line[..i],
        None => line,
    }
}

fn visit(dir: &Path, f: &mut impl FnMut(&Path, &str)) {
    let entries =
        std::fs::read_dir(dir).unwrap_or_else(|e| panic!("read_dir {}: {e}", dir.display()));
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            visit(&path, f);
        } else if path.extension().is_some_and(|e| e == "rs") {
            let text = std::fs::read_to_string(&path).expect("read source");
            f(&path, &text);
        }
    }
}
