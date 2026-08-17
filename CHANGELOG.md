# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [1.0.0] - 2026-08-17

### Added

- `jip` CLI with `init`, `add`, `remove`, `resolve`, `build`, `run`, `test`,
  `jar`, `search`, `tree`, and `update` commands.
- Dependency resolution from Maven Central and custom repositories
  (including `file://` local Maven-style folders).
- Lock file (`jip.lock`) pinning every dependency for reproducible builds,
  with three scopes: `packages`, `provided_packages`, and `test_packages`.
- Maven and Gradle conversion: `jip init` / `offer_conversion` maps Maven
  POMs and Gradle build scripts into `jip.toml` without touching the
  original files.
  - Runtime, test, and provided dependency scopes.
  - Custom repositories (including `${project.basedir}` / `$projectDir`
    resolution for local repos).
  - Java version detection from `maven-compiler-plugin` or Gradle
    `toolchain` / `sourceCompatibility` / `targetCompatibility`.
- Main class auto-detection with an interactive picker when several
  candidates exist (TTY only).
- Smart `jip run` argument handling: a positional that is not a main class
  becomes the first program argument (`jip run start`), JVM system
  properties pass through as `-DNAME=VALUE`, hyphen arguments require `--`.
- JAR packaging: `jip jar` builds a thin jar (`target/app.jar`), `jip jar --fat`
  merges all dependencies into an uber jar (`target/app-fat.jar`) with a
  last-wins duplicate resource policy and warning.
- JUnit test runner: compiles and executes tests via the JUnit Platform
  Console Launcher from `junit-platform-console-standalone`.
- `jip update` with an interactive confirmation prompt before applying
  version bumps (refuses to run without a terminal).
- M2 cache reuse (`use-m2 = true`) and `[classpath] extra` / `test-extra`
  entries.
- README, license metadata, and a cross-platform release workflow
  (Linux, Windows, macOS for x86_64 and aarch64).

[1.0.0]: https://github.com/autumoswitzerland/jip/releases/tag/v1.0.0