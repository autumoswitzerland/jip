// ------------------------------------------------------------------------------
// Copyright (c) 2026 autumo GmbH. All rights reserved.
//
// Licensed under the GNU Affero General Public License v3.0 (AGPLv3).
// See LICENSE file in the project root for full license information.
//
// This file is part of jip. jip is free software: you can redistribute
// it and/or modify it under the terms of the GNU Affero General Public License
// as published by the Free Software Foundation, either version 3 of the
// License, or (at your option) any later version.
// ------------------------------------------------------------------------------

// =============================================================================
//  jip — Commands
//  ---------------------------------------------------------------------------
//  Every subcommand lives in its own module here.  This file only provides
//  the shared plumbing: the HTTP client, config/lock loading and saving,
//  and the download step common to several commands.
//
//  Project:   jip
//  Author:    autumo GmbH
//  Date:      2026-08-15
// =============================================================================

pub mod add;
pub mod build;
pub mod clean;
pub mod completion;
pub mod get;
pub mod info;
pub mod init;
pub mod jar;
pub mod java;
pub mod list;
pub mod outdated;
pub mod remove;
pub mod resolve;
pub mod run;
pub mod search;
pub mod test;
pub mod tree;
pub mod update;

use std::io::{IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, bail};
use reqwest::blocking::Client;

use crate::artifact::Artifact;
use crate::cache::Cache;
use crate::config::{CONFIG_FILE, ProjectConfig};
use crate::lock::{LOCK_FILE, LockFile};
use crate::resolver::{DEFAULT_REPO_URL, Resolution, Resolver};

/// The jip version, taken from Cargo.toml at compile time.
const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Build the shared HTTP client with a sensible user agent, timeout,
/// and optional proxy configuration from env vars or `jip.toml`.
pub fn new_client() -> Client {
    let mut builder = Client::builder()
        .user_agent(format!("jip/{VERSION}"))
        .timeout(std::time::Duration::from_secs(60));

    // Read proxy from jip.toml, fall back to env vars
    let config = load_config().ok();
    let proxy_cfg = config.as_ref().and_then(|c| {
        if c.proxy.http_proxy.is_some() || c.proxy.https_proxy.is_some() {
            Some(&c.proxy)
        } else {
            None
        }
    });

    let http_proxy = proxy_cfg
        .and_then(|p| p.http_proxy.clone())
        .or_else(|| std::env::var("HTTP_PROXY").ok())
        .or_else(|| std::env::var("http_proxy").ok());

    let https_proxy = proxy_cfg
        .and_then(|p| p.https_proxy.clone())
        .or_else(|| std::env::var("HTTPS_PROXY").ok())
        .or_else(|| std::env::var("https_proxy").ok());

    if let Some(url) = http_proxy
        && let Ok(proxy) = reqwest::Proxy::http(&url)
    {
        builder = builder.proxy(proxy);
    }
    if let Some(url) = https_proxy
        && let Ok(proxy) = reqwest::Proxy::https(&url)
    {
        builder = builder.proxy(proxy);
    }

    builder
        .build()
        .expect("building the HTTP client never fails")
}

/// Load `jip.toml`, using defaults when the file does not exist yet.
pub fn load_config() -> anyhow::Result<ProjectConfig> {
    ProjectConfig::load(Path::new(CONFIG_FILE))
}

/// Load `jip.toml`, but fail when the project was not initialized yet.
pub fn require_config() -> anyhow::Result<ProjectConfig> {
    if !Path::new(CONFIG_FILE).exists() {
        bail!("no {CONFIG_FILE} found — run `jip init` first");
    }
    load_config()
}

/// The list of repositories to try, custom ones first and Maven Central last.
pub fn repositories_for(config: &ProjectConfig) -> Vec<String> {
    let mut repos: Vec<String> = config.repositories.values().cloned().collect();
    repos.push(DEFAULT_REPO_URL.to_string());
    repos
}

/// Run a full resolution for the project's runtime dependencies.
pub fn resolve(
    client: &Client,
    config: &ProjectConfig,
    offline: bool,
) -> anyhow::Result<Resolution> {
    let mut resolver = Resolver::new(client.clone(), &repositories_for(config), offline);
    resolver.resolve_project(config)
}

/// Run a full resolution for the project's test dependencies.
pub fn resolve_tests(
    client: &Client,
    config: &ProjectConfig,
    offline: bool,
) -> anyhow::Result<Resolution> {
    let mut resolver = Resolver::new(client.clone(), &repositories_for(config), offline);
    resolver.resolve_project_tests(config)
}

/// Run a full resolution for the project's compile-only (`provided`)
/// dependencies.
pub fn resolve_provided(
    client: &Client,
    config: &ProjectConfig,
    offline: bool,
) -> anyhow::Result<Resolution> {
    let mut resolver = Resolver::new(client.clone(), &repositories_for(config), offline);
    resolver.resolve_project_provided(config)
}

/// Write a new `jip.lock` from the runtime, compile-only, and test artifact
/// lists.
pub fn write_lock(
    artifacts: &[Artifact],
    provided: &[Artifact],
    test_artifacts: &[Artifact],
) -> anyhow::Result<()> {
    LockFile::from_artifacts(
        artifacts.to_vec(),
        provided.to_vec(),
        test_artifacts.to_vec(),
    )
    .save(Path::new(LOCK_FILE))
}

/// A cache configured from the project's settings.
pub fn cache_for(client: &Client, config: &ProjectConfig, offline: bool) -> Cache {
    Cache::new(client.clone(), config.cache.use_m2, offline)
}

/// Download any jar that is not yet available locally.
pub fn ensure_jars(cache: &Cache, artifacts: &[Artifact], repos: &[String]) -> anyhow::Result<()> {
    for artifact in artifacts {
        cache
            .ensure_jar(artifact, repos)
            .with_context(|| format!("cannot obtain {}", artifact.jar_file_name()))?;
    }
    Ok(())
}

/// The pinned runtime, compile-only, and test artifacts from `jip.lock`, or
/// a freshly resolved set when the lock file is missing (writing it back so
/// the project stays reproducible).
pub fn lock_parts(
    client: &Client,
    config: &ProjectConfig,
    offline: bool,
) -> anyhow::Result<(Vec<Artifact>, Vec<Artifact>, Vec<Artifact>)> {
    if let Some(lock) = LockFile::load(Path::new(LOCK_FILE))? {
        let runtime = lock.packages.iter().map(|p| p.to_artifact()).collect();
        let provided = lock
            .provided_packages
            .iter()
            .map(|p| p.to_artifact())
            .collect();
        let test = lock.test_packages.iter().map(|p| p.to_artifact()).collect();
        return Ok((runtime, provided, test));
    }
    let resolution = resolve(client, config, offline)?;
    let provided = resolve_provided(client, config, offline)?;
    let tests = resolve_tests(client, config, offline)?;
    write_lock(&resolution.flat, &provided.flat, &tests.flat)?;
    Ok((resolution.flat, provided.flat, tests.flat))
}

/// The pinned runtime artifacts from `jip.lock`.
pub fn locked_artifacts(
    client: &Client,
    config: &ProjectConfig,
    offline: bool,
) -> anyhow::Result<Vec<Artifact>> {
    Ok(lock_parts(client, config, offline)?.0)
}

/// The resolved runtime dependency jars, downloading anything that is not
/// cached yet, plus the configured `[classpath] extra` entries.
pub fn classpath_for(
    client: &Client,
    config: &ProjectConfig,
    offline: bool,
) -> anyhow::Result<Vec<PathBuf>> {
    let cache = cache_for(client, config, offline);
    let repos = repositories_for(config);
    let mut classpath = Vec::new();
    for artifact in locked_artifacts(client, config, offline)? {
        classpath.push(cache.ensure_jar(&artifact, &repos)?);
    }
    classpath.extend(classpath_extras(&config.classpath.extra));
    Ok(classpath)
}

/// The runtime and test dependency jars, downloading anything that is not
/// cached yet, plus the configured `[classpath]` entries.  This is the
/// classpath `jip test` works with.
pub fn test_classpath_for(
    client: &Client,
    config: &ProjectConfig,
    offline: bool,
) -> anyhow::Result<Vec<PathBuf>> {
    let cache = cache_for(client, config, offline);
    let repos = repositories_for(config);
    let (runtime, _, test) = lock_parts(client, config, offline)?;
    let mut classpath = Vec::new();
    for artifact in runtime.into_iter().chain(test) {
        classpath.push(cache.ensure_jar(&artifact, &repos)?);
    }
    let mut extras = config.classpath.extra.clone();
    extras.extend(config.classpath.test_extra.iter().cloned());
    classpath.extend(classpath_extras(&extras));
    Ok(classpath)
}

/// The compile-only (`provided`) dependency jars, from `[provided-dependencies]`.
///
/// These exist for `javac` only and never reach the classpath of a running
/// program, mirroring Maven's `provided` scope.
pub fn provided_classpath_for(
    client: &Client,
    config: &ProjectConfig,
    offline: bool,
) -> anyhow::Result<Vec<PathBuf>> {
    let cache = cache_for(client, config, offline);
    let repos = repositories_for(config);
    let mut classpath = Vec::new();
    for artifact in lock_parts(client, config, offline)?.1 {
        classpath.push(cache.ensure_jar(&artifact, &repos)?);
    }
    Ok(classpath)
}

/// The jars `javac` compiles against: runtime plus compile-only
/// (`provided`) dependencies.  This is the classpath used by
/// `jip build` and the compile step of `jip run`/`jip test`.
pub fn compile_classpath_for(
    client: &Client,
    config: &ProjectConfig,
    offline: bool,
) -> anyhow::Result<Vec<PathBuf>> {
    let mut classpath = classpath_for(client, config, offline)?;
    classpath.extend(provided_classpath_for(client, config, offline)?);
    Ok(classpath)
}

/// Turn configured classpath entries into paths, skipping empty strings.
fn classpath_extras(entries: &[String]) -> Vec<PathBuf> {
    entries
        .iter()
        .filter(|entry| !entry.is_empty())
        .map(PathBuf::from)
        .collect()
}

/// Join classpath entries with the platform's separator.
pub fn classpath_string(entries: &[PathBuf]) -> String {
    entries
        .iter()
        .map(|p| p.to_string_lossy().into_owned())
        .collect::<Vec<_>>()
        .join(classpath_separator())
}

pub fn classpath_separator() -> &'static str {
    if cfg!(windows) { ";" } else { ":" }
}

/// Check that the installed JDK is new enough for the project.
///
/// When the active JDK is too old but a matching JDK is already installed,
/// the user is asked (TTY only) whether to switch to it.  The JDK is never
/// activated without an explicit answer.
pub fn check_java_version(required: Option<&str>) -> anyhow::Result<()> {
    let Some(required) = required else {
        return Ok(());
    };
    let required_major = parse_major(required)
        .with_context(|| format!("invalid java version \"{required}\" in jip.toml"))?;
    let installed_major = java_major_version()?;
    if installed_major >= required_major {
        return Ok(());
    }

    // Find installed JDKs that satisfy the requirement.
    let candidates: Vec<crate::jdk::JdkInstallation> = crate::jdk::list_installed()?
        .into_iter()
        .filter(|j| parse_major(&j.version).unwrap_or(0) >= required_major)
        .collect();

    if candidates.is_empty() {
        bail!(
            "this project needs Java {required_major}, but the installed JDK is Java {installed_major} \
             (install a newer JDK and try again, e.g. `jip java install {required_major}`)"
        );
    }

    println!(
        "{}",
        crate::console::yellow(&format!(
            "this project needs Java {required_major}, but the active JDK is Java {installed_major}"
        ))
    );

    if !std::io::stdin().is_terminal() {
        let listed: Vec<String> = candidates
            .iter()
            .map(|j| format!("  - {} {}", j.vendor, j.version))
            .collect();
        bail!(
            "no active JDK matches — set one with `jip java use <version>`:\n{}",
            listed.join("\n")
        );
    }

    let chosen = pick_jdk(&candidates)?;
    crate::jdk::set_active(chosen.vendor, &chosen.version)?;
    Ok(())
}

/// Let the user pick one of several matching installed JDKs (numbered), or
/// confirm the only candidate with a yes/no prompt.
fn pick_jdk(
    candidates: &[crate::jdk::JdkInstallation],
) -> anyhow::Result<crate::jdk::JdkInstallation> {
    if candidates.len() == 1 {
        let j = &candidates[0];
        print!(
            "use {} {} as active JDK? [Y/n] ",
            crate::console::bold(&j.vendor.to_string()),
            crate::console::bold(&j.version)
        );
        std::io::stdout().flush().context("cannot write prompt")?;
        let mut answer = String::new();
        std::io::stdin()
            .read_line(&mut answer)
            .context("cannot read answer")?;
        let answer = answer.trim();
        if answer.is_empty()
            || answer.eq_ignore_ascii_case("y")
            || answer.eq_ignore_ascii_case("yes")
        {
            return Ok(j.clone());
        }
        bail!("aborted — set a JDK with `jip java use <version>`");
    }

    println!(
        "installed JDKs matching Java {}:",
        parse_major(&candidates[0].version).unwrap_or(0)
    );
    for (index, j) in candidates.iter().enumerate() {
        println!("  {}. {} {}", index + 1, j.vendor, j.version);
    }
    let stdin = std::io::stdin();
    loop {
        print!("  select 1-{} (q to quit): ", candidates.len());
        std::io::stdout().flush().context("cannot write prompt")?;
        let mut answer = String::new();
        if stdin
            .read_line(&mut answer)
            .context("cannot read selection")?
            == 0
        {
            bail!("aborted");
        }
        let answer = answer.trim();
        if answer.eq_ignore_ascii_case("q") || answer.is_empty() {
            bail!("aborted — set a JDK with `jip java use <version>`");
        }
        if let Ok(index) = answer.parse::<usize>()
            && let Some(j) = candidates.get(index.wrapping_sub(1))
        {
            return Ok(j.clone());
        }
    }
}

/// Query the major Java version — uses the active JDK if set, otherwise `java` from PATH.
pub fn java_major_version() -> anyhow::Result<u32> {
    let java_path = java_binary()?;
    let output = Command::new(&java_path)
        .arg("-version")
        .output()
        .with_context(|| format!("failed to run {} -version", java_path.display()))?;
    let stderr = String::from_utf8_lossy(&output.stderr);
    let quoted = stderr
        .split('"')
        .nth(1)
        .context("cannot parse `java -version` output")?;
    parse_major(quoted)
}

/// Get the path to the `java` binary — active JDK first, then PATH.
fn java_binary() -> anyhow::Result<PathBuf> {
    if let Ok(path) = crate::jdk::active_java() {
        return Ok(path);
    }
    which_java()
}

/// Get the path to the `javac` binary — active JDK first, then PATH.
pub fn javac_binary() -> anyhow::Result<PathBuf> {
    if let Some(active) = crate::jdk::ActiveConfig::load().ok().and_then(|c| c.active) {
        let base = crate::jdk::jdk_base()?;
        let jdk_dir = base.join(active.vendor.to_string()).join(&active.version);
        let exe = crate::jdk::with_exe("javac");

        let standard = jdk_dir.join("bin").join(&exe);
        if standard.exists() {
            return Ok(standard);
        }
        let macos = jdk_dir.join("Contents/Home/bin").join(&exe);
        if macos.exists() {
            return Ok(macos);
        }
    }
    which("javac")
}

/// Find `java` on PATH.
fn which_java() -> anyhow::Result<PathBuf> {
    which("java").context("no `java` on PATH — install a JDK or run `jip java install <version>`")
}

/// Find a binary on PATH.
fn which(name: &str) -> anyhow::Result<PathBuf> {
    let cmd = if cfg!(windows) { "where" } else { "which" };
    let output = Command::new(cmd)
        .arg(name)
        .output()
        .with_context(|| format!("failed to run `{cmd} {name}`"))?;
    if output.status.success() {
        let stdout = String::from_utf8_lossy(&output.stdout);
        let path = stdout
            .lines()
            .map(|l| l.trim())
            .find(|l| !l.is_empty())
            .context("command returned empty output")?;
        Ok(PathBuf::from(path))
    } else {
        bail!("`{name}` not found on PATH")
    }
}

/// Extract the major version number, e.g. `21.0.2` -> 21 and `1.8.0_392` -> 8.
pub fn parse_major(version: &str) -> anyhow::Result<u32> {
    let first = version.split(['.', '_', '-']).next().unwrap_or_default();
    if first == "1" {
        // Legacy numbering: 1.8 means Java 8.
        let second = version.split('.').nth(1).unwrap_or_default();
        return second
            .parse::<u32>()
            .with_context(|| format!("cannot parse java version \"{version}\""));
    }
    first
        .parse::<u32>()
        .with_context(|| format!("cannot parse java version \"{version}\""))
}
