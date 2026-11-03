# mailctl Agent Guide

## Project

`mailctl` is an Apple Silicon macOS CLI written in Rust for Apple Mail. Reads
query Mail's local `Envelope Index` database and `.emlx` files; writes run the
embedded JXA bridge in `src/jxa/mail.js` through `osascript`. Human-readable
output is the default; `--json` is the scripting and agent interface.

Writes change the user's real mailboxes and sync to their mail servers.
Preserve the dry-run, confirmation, and no-permanent-delete safeguards
documented in `SKILL.md`. When testing writes against a live Mail.app, use
messages the user has agreed to, restore their state afterwards, and never
send mail.

[docs/architecture.md](docs/architecture.md) records the store layout and
Mail.app scripting behavior the code depends on. Update it when you discover
or rely on new behavior.

## Agent Skill

The repository-root `SKILL.md` is the bundled agent skill, a single
self-contained file. `README.md` documents how to install it. Keep both in
step with the command surface.

## Commit Hooks

Commit hooks are defined in `.pre-commit-config.yaml` and run with `prek`.
Install the runner once with `uv tool install prek`, then activate the hooks in
this checkout with `prek install`.

`.github/workflows/ci.yml` runs that same configuration rather than restating
its commands. Apply the checks outside a commit with `prek run --all-files`
and `prek run --all-files --hook-stage pre-push`; clippy and tests run at the
push stage.

Development is macOS-only. Tests use temporary fixtures and never touch the
live Mail store or Mail.app.

## Git Commit Prefixes

Use Conventional Commit-style subjects in the form
`prefix: concise imperative summary`, with a lowercase prefix, no trailing
period, and an optional scope only when it clarifies the subject:

- `feat:` new user-facing functionality
- `fix:` bug fixes or correctness changes
- `docs:` documentation-only changes
- `test:` test-only additions or corrections
- `refactor:` internal restructuring without a behavior change
- `perf:` performance improvements
- `build:` dependency or build-system changes
- `ci:` continuous-integration changes
- `chore:` repository maintenance that fits none of the above

## Releases

Do not bump the version without explicit user approval. Use Semantic
Versioning: patch for compatible fixes, minor for new functionality or
breaking changes while pre-1.0. Tag approved releases as `v<version>` with an
annotated tag, and ask for approval immediately before pushing the tag.
