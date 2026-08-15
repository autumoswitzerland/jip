# AGENTS.md

## Project

**jip** — a pip-like dependency manager and runner for Java, written in Rust.
Distributed as a single binary. License: AGPL-3.0-only, copyright `autumo GmbH`.

All work happens in `/Users/Mike/Development/OpenCode/jip` (the workspace root).
Demo/test project dirs live under `/tmp/jip-*` and have **no** Cargo.toml — cargo
commands must use the project root as workdir.

## Commands

```sh
./bin/make.sh            # build jip for the current platform (release; --debug)
./bin/test.sh            # fmt check + clippy + tests (20 tests green, keep them green)
./bin/test.sh --fast     # tests only
./bin/release.sh <ver>   # tag v<ver> + push, triggers .github/workflows/release.yml
```

Direct cargo equivalents:

```sh
cargo build          # build
cargo test           # run tests (20 tests green, keep them green)
cargo clippy         # must be 0 warnings
cargo fmt --check    # must be clean
```

`source $HOME/.cargo/env` is required before using cargo. `brew` is NOT installed
on this host; Java 21 Zulu at `/usr/bin/java` (includes `javac`).

## Conventions

- No code comments unless asked. Header/banner style follows
  `/Users/Mike/Development/OpenCode/WebDuck/src/webduck/config.py`.
- `jip.toml` in project, `jip.lock` pinned/committed. Never `publish`.
- Clone → `jip run` must work with zero extra steps (lazy downloads, auto-detect
  main class, conversion offer for foreign Maven/Gradle repos).
- Build middle-way, not a full build system: `jip build` (javac → `target/classes`),
  lazy `jip run` compile, and `jip test` (compiles `src/test/java` → `target/test-classes`
  and runs the JUnit Platform Console Launcher from `junit-platform-console-standalone`).
- Dependencies are split into `[dependencies]` (runtime, for `jip run`/`jip build`) and
  `[test-dependencies]` (`jip add <dep> --test`, only on the `jip test` classpath).
  `jip.lock` pins both in `packages` and `test_packages` (format version 2).
- `[project] source` optional (default `src/main/java`, Maven layout).
  `[project] main` optional as FQCN override only.
- Conversion is offer-only (`offer_conversion`), never automatic. Original
  Maven/Gradle files stay untouched (additive only).
- Name stays **`jip`** — no renaming. Distribution channels are package managers,
  never pip / never `cargo install` as the mainstream path.

## Package Manager TODO (for later, not blocking)

Name `jip` verified **free** on all these channels — check again before each release:

| Channel | Status | Action later |
|---|---|---|
| Homebrew formula | free | create tap + formula |
| winget ID | free | submit manifest |
| Scoop manifest | free | submit manifest |
| Chocolatey | free | submit package |
| apt (Debian/Ubuntu) | free (no exact `jip`) | package .deb |
| dnf (Fedora/RHEL) | free | package .rpm |
| Snap | free (only substring hits) | snapcraft |
| pacman/AUR | free | AUR package |
| Nix/nixpkgs | free (nothing known) | nixpkgs PR (optional) |

**crates.io:** user reserves the name `jip` themselves (create account → `cargo login`
→ `cargo publish` placeholder v0.1.0). Not a cargo-install distribution path — a
pure hedge against a future convenience channel. Note: the foreign `jip-cli` project
(crates.io, Linux networking, Apr 2026) holds 16 `jip-*` crates; the plain `jip`
name is still free. If we ever need sub-crates (`jip-lib`, `jip-core`, …) those
names are taken.
