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

The public surfaces are the command-line interface (commands, flags, and exit
codes) and the `--json` output. Do not bump the version on every commit or
without explicit user approval. Use Semantic Versioning: patch for compatible
fixes, minor for new functionality or breaking changes while pre-1.0, and
major for breaking changes after 1.0. When a coherent, tested, documented set
of changes is substantial enough to release, suggest the version and rationale
to the user.

`Cargo.toml` is the version source. A release-preparation commit changes its
version, the matching `CFBundleShortVersionString` and `CFBundleVersion` in
`Info.plist` (a test checks they agree), and `Cargo.lock` after `cargo build`.

Publishing a release is one action. Ask for explicit approval once, immediately
before pushing the release tag, presenting the exact version, the commit it will
point at, and the rationale for that number. That approval covers publishing the
GitHub Release from that tag. Ask again only for a different version or commit,
or to modify or delete a release that already exists. Approval to edit, commit,
or push ordinary commits is not release approval.

Create a matching annotated Git tag for every approved release using
`v<version>`, for example `v0.1.0`, and always push that tag explicitly to
`origin` after creating it. The GitHub Release, titled with the tag, holds the
release notes: user-visible changes grouped as added, changed, fixed, and
breaking.

Never move or replace an existing release tag. A repository ruleset named
`protect-release-tags` enforces this on the remote for `refs/tags/v*`, denying
tag deletion, non-fast-forward updates, and updates, with no bypass. A rejected
tag push is that rule working, not a broken remote.
