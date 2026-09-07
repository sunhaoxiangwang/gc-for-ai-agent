//! Running command reclaimers.
//!
//! Some targets belong to a tool with its own garbage collector, and asking
//! that tool to clean up is both safer and more correct than removing its
//! directories. Docker's build cache is the clearest example: its layout is
//! private, and removing files out from under the daemon corrupts it.
//!
//! Guards 4 through 8 cannot apply here, because there is no path to guard. The
//! compensating control is that the argv array is a literal from the config
//! file, validated at load time to contain no substitution syntax, and executed
//! directly rather than through a shell.

use std::process::Command;

use reap_core::schema::CommandReport;
use tracing::info;

use crate::context::Context;
use crate::executor::TimeBudget;

/// Runs every command rule in the report, in tier order.
///
/// Returns how many ran and any errors. A command that fails is reported, not
/// fatal: `docker` not being installed on a machine whose config mentions it is
/// a normal thing that should not abort the rest of a sweep.
pub fn run_command_rules(
    ctx: &Context,
    commands: &mut [CommandReport],
    budget: &TimeBudget,
) -> (usize, Vec<String>) {
    let mut ran = 0;
    let mut errors = Vec::new();

    commands.sort_by_key(|c| c.tier);
    for c in commands.iter_mut() {
        if budget.exhausted() {
            break;
        }
        info!(rule = %c.rule, tier = c.tier, argv = ?c.argv, "running command reclaimer");

        let Some((program, args)) = c.argv.split_first() else {
            continue;
        };
        match Command::new(program).args(args).output() {
            Err(e) => {
                c.error = Some(format!("could not run {program}: {e}"));
                errors.push(format!("{}: could not run {program}: {e}", c.rule));
            }
            Ok(out) => {
                c.executed = true;
                c.exit_code = out.status.code();
                ran += 1;
                let stdout = String::from_utf8_lossy(&out.stdout);
                c.reclaimed_bytes = parse_reclaimed(&stdout);
                if !out.status.success() {
                    let stderr = String::from_utf8_lossy(&out.stderr);
                    let detail = stderr.trim();
                    let msg = format!(
                        "{} exited {}{}",
                        c.argv.join(" "),
                        out.status.code().unwrap_or(-1),
                        if detail.is_empty() {
                            String::new()
                        } else {
                            format!(": {detail}")
                        }
                    );
                    c.error = Some(msg.clone());
                    errors.push(format!("{}: {msg}", c.rule));
                }
            }
        }
        let _ = ctx;
    }
    (ran, errors)
}

/// Extracts a reclaimed byte count from a tool's own output.
///
/// Docker prints `Total reclaimed space: 1.234GB`; pnpm and go print nothing
/// useful. Returning `None` is the honest answer for the rest, and the report
/// says "size not reported" rather than inventing a zero.
pub fn parse_reclaimed(stdout: &str) -> Option<u64> {
    let line = stdout
        .lines()
        .find(|l| l.to_ascii_lowercase().contains("reclaimed space"))?;
    let value = line.rsplit(':').next()?.trim();
    parse_size(value)
}

/// Parses a size the way Docker writes it: a decimal number and a unit, with
/// SI-style suffixes that are actually powers of 1024.
fn parse_size(s: &str) -> Option<u64> {
    let s = s.trim();
    let split = s.find(|c: char| c.is_alphabetic())?;
    let (number, unit) = s.split_at(split);
    let number: f64 = number.trim().parse().ok()?;
    let scale: f64 = match unit.trim().to_ascii_uppercase().as_str() {
        "B" => 1.0,
        "KB" | "KIB" => 1024.0,
        "MB" | "MIB" => 1024.0 * 1024.0,
        "GB" | "GIB" => 1024.0 * 1024.0 * 1024.0,
        "TB" | "TIB" => 1024.0 * 1024.0 * 1024.0 * 1024.0,
        _ => return None,
    };
    Some((number * scale) as u64)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_dockers_reclaimed_line() {
        let out = "Deleted build cache objects:\nabc123\n\nTotal reclaimed space: 1.234GB\n";
        assert_eq!(parse_reclaimed(out), Some(1_324_997_410));
    }

    #[test]
    fn returns_none_rather_than_zero_when_the_tool_says_nothing() {
        assert_eq!(parse_reclaimed(""), None);
        assert_eq!(parse_reclaimed("removed 4 packages\n"), None);
        // A line that mentions the phrase but carries no parseable size.
        assert_eq!(parse_reclaimed("Total reclaimed space: lots\n"), None);
    }

    #[test]
    fn handles_every_unit_docker_emits() {
        assert_eq!(parse_size("0B"), Some(0));
        assert_eq!(parse_size("512B"), Some(512));
        assert_eq!(parse_size("1.5KB"), Some(1536));
        assert_eq!(parse_size("2MB"), Some(2 * 1024 * 1024));
        assert_eq!(parse_size("3.25GB"), Some(3_489_660_928));
    }
}
