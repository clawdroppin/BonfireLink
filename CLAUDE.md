# CLAUDE.md

Read **[AGENTS.md](AGENTS.md)** first: it has the ground rules, module map, mod facts, testing and release steps.

Quick reminders:
- `cargo test`, `cargo clippy --all-targets` and `cargo fmt` must be clean before you finish.
- Use `BONFIRELINK_DATA=<temp dir>` for any manual run so the user's real `BonfireLink-data/` is untouched.
- Never edit files in a real game folder or Steam's config during development; use a fake game folder.
- Keep the Linux build free of new system libraries (`python -I scripts/check_elf.py <binary>`).
- Update the CHANGELOG and the "Verification status" list in AGENTS.md when behaviour changes.
