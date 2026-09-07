# Security

## Reporting a vulnerability

Report privately, not in a public issue.

Use GitHub's private vulnerability reporting on this repository: the **Security**
tab, then **Report a vulnerability**. That opens a private thread with the
maintainers.

Please include what you were running, the configuration that triggered it, and
what happened. A proof of concept helps enormously; a description of the class
of problem is fine if you would rather not write one.

You should get an acknowledgement within a week. If you do not, open a public
issue saying only that you are waiting on a private report, with no details.

## Supported versions

Pre-1.0. Only the most recent release is supported. Fixes go into a new patch
release rather than being backported.

## Threat model

`reap` deletes files on a schedule, so the interesting failures are all about
deleting the wrong ones.

- **In scope, highest severity:** any path-handling bug that lets `reap` remove
  something outside a declared root, or something the guard stack should have
  refused. Symlink handling, canonicalization, containment, and the quarantine
  rename are the places to look.
- **In scope:** a way to make `reap` execute a command that is not literally in
  the config file; a way to make the two-opt-in gate pass with only one opt-in;
  a way to make a guard return "pass" on an error path.
- **Out of scope:** an attacker who can already write to your config file or
  your `PATH` has already won, and `reap` does not try to defend against that. A
  rule you wrote that is broader than you intended is a bug in your config, not
  in `reap`, though a case where the guards should plausibly have caught it is
  worth reporting.

`reap` makes no network calls during normal operation and collects no telemetry.
