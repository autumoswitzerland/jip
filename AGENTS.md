# AGENTS.md

## Project

**jip** — a pip-like dependency manager and runner for Java, written in Rust.
Distributed as a single binary. License: AGPL-3.0-only, copyright `autumo GmbH`.

All work happens in the workspace root (this repository). Demo/test project
dirs live under `/tmp/jip-*` and have **no** Cargo.toml — cargo commands must
use the repository root as workdir.

## Commands

```sh
./bin/make.sh            # build jip for the current platform (release; --debug)
./bin/test.sh            # fmt check + clippy + tests (all tests green, keep them green)
./bin/test.sh --fast     # tests only
./bin/release.sh <ver>   # tag v<ver> + push, triggers .github/workflows/release.yml
```

Direct cargo equivalents:

```sh
cargo build          # build
cargo test           # run tests
cargo clippy         # must be 0 warnings
cargo fmt --check    # must be clean
```

`source $HOME/.cargo/env` is required before using cargo. `brew` is NOT installed
on this host; Java 21 Zulu at `/usr/bin/java` (includes `javac`).

## Conventions

- **Cross-platform first:** jip must work identically on macOS, Linux, and Windows.
  Never use hardcoded `/` or `\` in path construction — always use `Path::join` /
  `PathBuf`. Never shell out to platform-specific commands (`tar`, `which`, `rm`)
  when a Rust crate exists. Check `cfg!(windows)` for OS-specific behavior (`.exe`
  suffixes, archive formats, path separators, ANSI support). All tests must pass
  on all three platforms.
- No code comments unless asked. Header/banner style follows the AGPL header +
  section banner used across `src/` (any file header is the model).
- `jip.toml` in project, `jip.lock` pinned/committed. Never `publish`.
- Clone → `jip run` must work with zero extra steps (lazy downloads, auto-detect
  main class, conversion offer for foreign Maven/Gradle repos).
- Build middle-way, not a full build system: `jip build` (javac → `target/classes`),
  lazy `jip run` compile, `jip jar` / `jip jar --fat` (packages into `target/app.jar`
  or `target/app-fat.jar` with all dependencies merged), and `jip test` (compiles
  `src/test/java` → `target/test-classes` and runs the JUnit Platform Console Launcher
  from `junit-platform-console-standalone`).
- `jip get <url>` clones a repository into `./<repo-name>/` and converts a detected
  Maven/Gradle build to `jip.toml` non-interactively — the conversion is automatic
  because the clone is throwaway. It does NOT execute the project: running freshly
  cloned code is a trust decision, so it prints `cd <name> && jip run` as the next
  step instead. A missing `git` fails with a clear message.
- Dependencies are split into `[dependencies]` (runtime, for `jip run`/`jip build`),
  `[provided-dependencies]` (compile-only, `jip add <dep> --provided`) and
  `[test-dependencies]` (`jip add <dep> --test`, only on the `jip test` classpath).
  `jip.lock` pins all three in `packages`, `provided_packages` and `test_packages`
  (format version 3).
- Custom repositories live in `[repositories]` (id = URL), tried before Maven Central.
  URLs may be `https://...` or `file://...` (local Maven-style folder); Maven's
  `${project.basedir}` is resolved against the project dir during conversion.
- Conversion (`jip init`/`offer_conversion`) maps Maven `compile`/Gradle runtime configs
  to `[dependencies]`, Maven `provided` scope / Gradle `compileOnly`/`compileOnlyApi`
  to `[provided-dependencies]` (javac only, never on the runtime classpath),
  `test` scope / `testImplementation` to `[test-dependencies]`,
  and `<repositories>` / Gradle `maven { url = ... }` to `[repositories]`;
  optional/system deps are skipped.
- `[project] source` optional (default `src/main/java`, Maven layout).
  `[project] main` optional as FQCN override only. With several `main`
  candidates `jip run`/`jip init` show a numbered picker (TTY only) and the
  chosen FQCN is written straight into `[project] main`; without a terminal
  the ambiguity is an error/warning with the candidate list.
- `jip run` argument handling: the first positional is a main class only if
  it is one (configured `[project] main`, detected `main` class, or
  `.java`/`.jar` file); otherwise it becomes the first program argument, so
  `jip run start` runs `[project] main` with `start`. Hyphen args need `--`
  (`jip run -- -h`), clap's own help still wins over a bare `-h`. JVM system
  properties pass through as `-DNAME=VALUE` (repeatable, `--define` works
  too) and are handed to `java` before the main class; there is no `-cp`
  passthrough — jip builds the classpath itself.
- `[project] java` is never hardcoded on init: a Maven project's Java version
  is carried over from the `maven-compiler-plugin` (`<release>`, then
  `<source>`) or the `maven.compiler.*`/`java.version` properties; otherwise
  the installed JDK major version on `PATH` is used (fallback "21").
- `[classpath]` optional: `extra` (dirs/jars on the `build`/`run`/`test`
  classpath, relative to the project root) and `test-extra` (only `jip test`).
  Never auto-converted from Maven plugin config (e.g. surefire
  `additionalClasspathElement`) — those are project-specific, set manually.
- Conversion is offer-only (`offer_conversion`) for the working directory; original
  Maven/Gradle files stay untouched (additive only). The exception is `jip get`,
  which converts automatically after cloning.
- **Multi-module:** `jip init` detects Maven parent POMs (via `<modules>`)
  and Gradle multi-project builds (via `settings.gradle` `include`). Creates
  a root `jip.toml` with `[modules]` section and per-module `jip.toml` files.
  Inter-module deps (Maven sibling artifacts, Gradle `project(':...')`) are
  excluded from external resolution and resolved from compiled classes.
  `jip build` compiles in topological order (deterministic: alphabetical
  tie-break at the same level); `jip run` flattens all module classes onto
  the classpath. Gradle `subprojects {}`/`allprojects {}` dependencies are
  propagated to child modules.
- Name stays **`jip`** — no renaming. Distribution channels are package managers,
  never pip / never `cargo install` as the mainstream path.

## JDK vendor download patterns (verified 2026-08-18)

| Vendor | Pattern |
|---|---|
| **Azul Zulu** | Metadata API: `https://api.azul.com/metadata/v1/zulu/packages/?java_version={version}&os={os}&arch={arch}&archive_type=tar.gz&java_package_type=jdk&latest=true&release_status=ga` |
| **Eclipse Temurin** | `https://api.adoptium.net/v3/binary/latest/{version}/ga/{os}/{arch}/jdk/hotspot/normal/eclipse` |
| **Amazon Corretto** | `https://corretto.aws/downloads/latest/amazon-corretto-{version}-{arch}-{os}-jdk.tar.gz` |
| **GraalVM CE** | `https://github.com/graalvm/graalvm-ce-builds/releases/download/jdk-{version}/graalvm-jdk-{version}_{os}-{arch}.tar.gz` |
| **BellSoft Liberica** | Product Discovery API: `https://api.bell-sw.com/v1/liberica/releases?version-feature={version}&bitness=64&os={os}&arch={arch}&package-type=tar.gz&bundle-type=jdk&version-modifier=latest` (arch: `x86`=x64, `arm`=aarch64) |

Custom vendor URLs: `~/.jip/jdk.toml` (key = vendor name, value = URL template with `{version}`, `{arch}`, `{os}` placeholders). Oracle JDK excluded (login required).