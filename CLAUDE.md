# Lejce — notes for Claude

Lejce is a Rust library that runs coding agents (Claude Code, pi, Codex)
for Kuterlapa and Lemat. `docs/design.md` holds the plan.
Expect the author's messages to come from dictation, with words misheard.

## The repository is public

- No private data in files, file names or commit messages: no host names,
  internal domains, IP addresses, home paths, people's names or personal
  settings. Use generic examples ("server", "~/notes").
- `make hooks` installs a pre-commit hook (`scripts/check-private.sh`). It
  checks staged changes against private patterns kept outside the
  repository. Do not bypass it with `--no-verify`.

## Conventions

- Code, comments and documentation in English.
- Comments explain why, not what.
- Keep the library free of any one program's needs: no Kuterlapa or Lemat
  settings, paths or user-facing text.
- Commits:
  - subject in imperative mood, under about 70 characters, with a module
    prefix: `host:`, `run:`, `activity:`, `mcp:`, `claude:`, `pi:`,
    `codex:`, `examples:`, `build:`, `docs:`;
  - the body explains why;
  - one commit per module or concern;
  - no AI attribution in commits (no `Co-Authored-By`, no tool names).
    Credit lives in `README.md`.
