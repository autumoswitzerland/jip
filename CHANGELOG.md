# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [1.0.0] - 2026-08-18

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
- Multi-module project support:
  - Automatic detection of Maven multi-module projects (parent POM with
    `<modules>`) and Gradle multi-project builds (`settings.gradle` with
    `include`).
  - Root `jip.toml` with `[modules]` section listing each module and its
    relative path.
  - Per-module `jip.toml` files with independently converted dependencies.
  - Inter-module dependency detection: Maven sibling artifacts (same
    groupId + artifactId match) and Gradle `project(':...')` references
    are excluded from external resolution.
  - `jip build` compiles modules in topological (dependency) order.
  - `jip run` flattens all module classes onto the classpath.
  - `jip jar` / `jip jar --fat` merge all module classes (and their
    dependencies) into a single thin or uber jar.
  - Gradle `subprojects {}` / `allprojects {}` dependency and repository
    propagation to child modules (module-local versions override inherited).
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
- `jip clean` — removes `target/` build artifacts.
- `jip list` — lists all resolved dependencies with pinned versions from
  `jip.lock`, grouped by scope.
- `jip outdated` — read-only check showing which direct dependencies have
  newer versions available on Maven Central.
- `jip update [<dep>]` — updates one or all direct dependencies to the
  latest version, with interactive confirmation (refuses without a terminal).
- `jip completion <shell>` — prints bash/zsh/fish completions to stdout.
- `jip info <dep>` — show metadata for a dependency: latest version,
  description, name, URL, and license (from the POM).
- `jip java` — manage JDK installations from the command line:
  - `jip java list` — list installed JDKs.
  - `jip java install <version> [--vendor <name>]` — download and install a JDK.
  - `jip java use <version> [--vendor <name>]` — set the active JDK.
  - `jip java remove <version> [--vendor <name>]` — remove an installed JDK.
  - Supported vendors: `zulu` (default), `temurin`, `corretto`, `graalvm`.
- `jip java remove` clears the active JDK config when the last installed
  JDK is removed, falling back to the system `java` on PATH.
- BOM / dependency-management import during conversion:
  - Maven `<dependencyManagement>` imports (`<type>pom</type><scope>import</scope>`)
    are downloaded and their managed versions applied to version-less
    `<dependencies>`.  Last import wins (Maven semantics).
  - Gradle `platform(...)` and `enforcedPlatform(...)` BOMs are resolved
    and applied to version-less dependencies.  Runtime platforms also
    apply to test dependencies (Gradle configuration inheritance).
- Gradle version catalog support (`gradle/libs.versions.toml`):
  - `[libraries]` entries with `group`, `name` (or `artifact`), and
    `version` (or `version.ref` into `[versions]`).
  - `libs.<alias>` accessor syntax in build scripts is resolved to
    coordinates, with camelCase-to-kebab fallback.
- Version priority (highest wins):
  1. Explicit `group:artifact:version` in the declaration
  2. Version catalog (`libs.x` accessor)
  3. `platform(...)` / Maven BOM
  4. `latest_version` fallback (in `convert_to_config`)
- Fat-jar fix: signature files (`META-INF/*.SF`, `*.RSA`, `*.DSA`) and
  duplicate metadata (`NOTICE`, `LICENSE`, `DEPENDENCIES`) are now excluded
  when merging dependency JARs.
- GraalVM CE download URL resolver reports a clear error when no release
  asset exists for the current platform (e.g. macOS Intel).
- `--offline` global flag: use only locally cached jars; fail when a
  dependency is not cached instead of downloading it.
- Proxy support: HTTP/HTTPS proxy via `[proxy]` section in `jip.toml`
  (`http-proxy`, `https-proxy`) or `HTTP_PROXY` / `HTTPS_PROXY` env vars.
  `NO_PROXY` env var is respected by reqwest automatically.
- README, license metadata, and a cross-platform release workflow
  (Linux, Windows, macOS for x86_64 and aarch64).

### Changed

- `collect_dependencies` now takes a `&reqwest::blocking::Client` to
  download BOM/platform POMs during conversion.
- Maven `PomDependency` carries an optional `typ` field (`<type>` element)
  to detect BOM imports (`type=pom`).
- Multi-module robustness:
  - `jip run` now lazily compiles every module in dependency order before
    starting, like `jip build` does (no more `ClassNotFoundException` on a
    fresh clone).
  - `jip build` / `jip jar` skip modules without `.java` sources
    (aggregator/BOM/parent modules) instead of failing.
  - Multi-module detection ignores `include`/`<module>` entries whose
    directory does not exist on disk.
  - Missing Java versions now explain the BOM-flattening limitation and
    what to do (`jip java install`, `dependencyManagement`).
  - The multi-module build order is deterministic: modules at the same
    dependency level are built in alphabetical order instead of a
    hash-map-dependent order that changed between runs.
- JDK selection: when the project needs a newer Java than the active JDK
  but a matching JDK is already installed, `jip run`/`jip build` tell the
  user ("this project needs Java X") and ask (TTY only) whether to switch
  (`use zulu 19 as active JDK? [Y/n]`). The JDK is never activated without
  an explicit answer.
- Clear messages for unsupported cases:
  - Kotlin sources (`src/main/kotlin`) are reported as not supported when
    no Java sources or main class are found.
  - Gradle version catalog bundles (`libs.bundles.*`) warn that the bundle
    is not resolved and the libraries must be added manually.
  - A Maven parent POM (`packaging=pom`) without a detectable module
    structure (modules in a profile or nested aggregator) warns that the
    project was converted as a single module and why.
  - Gradle subprojects included dynamically at run time (no static `include`
    statements) warn that jip cannot detect them and converted the project
    as a single module.
- `jip get <url>` — clone a git repository into `./<repo-name>/` and run it
  in one step. Shallow clones by default (`--depth 1`), checks the directory
  name first (TTY prompt) like `git clone`, converts a detected Maven/Gradle
  build to `jip.toml` automatically (no prompt, since clones are throwaway),
  passes arguments after `--` to the program, and honours the global
  `--offline` flag. Repositories without a Maven/Gradle build are refused
  with a clear error and the cloned directory is left in place.

[1.0.0]: https://github.com/autumoswitzerland/jip/releases/tag/v1.0.0
