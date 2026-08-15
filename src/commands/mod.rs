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
pub mod init;
pub mod remove;
pub mod resolve;
pub mod run;
pub mod search;
pub mod test;
pub mod tree;
pub mod update;

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

/// Build the shared HTTP client with a sensible user agent and timeout.
pub fn new_client() -> Client {
    Client::builder()
        .user_agent(format!("jip/{VERSION}"))
        .timeout(std::time::Duration::from_secs(60))
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

/// Run a full resolution for the project's runtime dependencies.
pub fn resolve(client: &Client, config: &ProjectConfig) -> anyhow::Result<Resolution> {
    let mut resolver = Resolver::new(client.clone(), DEFAULT_REPO_URL);
    resolver.resolve_project(config)
}

/// Run a full resolution for the project's test dependencies.
pub fn resolve_tests(client: &Client, config: &ProjectConfig) -> anyhow::Result<Resolution> {
    let mut resolver = Resolver::new(client.clone(), DEFAULT_REPO_URL);
    resolver.resolve_project_tests(config)
}

/// Write a new `jip.lock` from the runtime and test artifact lists.
pub fn write_lock(artifacts: &[Artifact], test_artifacts: &[Artifact]) -> anyhow::Result<()> {
    LockFile::from_artifacts(artifacts.to_vec(), test_artifacts.to_vec()).save(Path::new(LOCK_FILE))
}

/// A cache configured from the project's settings.
pub fn cache_for(client: &Client, config: &ProjectConfig) -> Cache {
    Cache::new(client.clone(), config.cache.use_m2)
}

/// Download any jar that is not yet available locally.
pub fn ensure_jars(cache: &Cache, artifacts: &[Artifact]) -> anyhow::Result<()> {
    for artifact in artifacts {
        cache
            .ensure_jar(artifact, DEFAULT_REPO_URL)
            .with_context(|| format!("cannot obtain {}", artifact.jar_file_name()))?;
    }
    Ok(())
}

/// The pinned runtime and test artifacts from `jip.lock`, or a freshly
/// resolved set when the lock file is missing (writing it back so the
/// project stays reproducible).
fn lock_parts(
    client: &Client,
    config: &ProjectConfig,
) -> anyhow::Result<(Vec<Artifact>, Vec<Artifact>)> {
    if let Some(lock) = LockFile::load(Path::new(LOCK_FILE))? {
        let runtime = lock.packages.iter().map(|p| p.to_artifact()).collect();
        let test = lock.test_packages.iter().map(|p| p.to_artifact()).collect();
        return Ok((runtime, test));
    }
    let resolution = resolve(client, config)?;
    let tests = resolve_tests(client, config)?;
    write_lock(&resolution.flat, &tests.flat)?;
    Ok((resolution.flat, tests.flat))
}

/// The pinned runtime artifacts from `jip.lock`.
pub fn locked_artifacts(client: &Client, config: &ProjectConfig) -> anyhow::Result<Vec<Artifact>> {
    Ok(lock_parts(client, config)?.0)
}

/// The resolved runtime dependency jars, downloading anything that is not
/// cached yet.
pub fn classpath_for(client: &Client, config: &ProjectConfig) -> anyhow::Result<Vec<PathBuf>> {
    let cache = cache_for(client, config);
    let mut classpath = Vec::new();
    for artifact in locked_artifacts(client, config)? {
        classpath.push(cache.ensure_jar(&artifact, DEFAULT_REPO_URL)?);
    }
    Ok(classpath)
}

/// The runtime and test dependency jars, downloading anything that is not
/// cached yet.  This is the classpath `jip test` works with.
pub fn test_classpath_for(client: &Client, config: &ProjectConfig) -> anyhow::Result<Vec<PathBuf>> {
    let cache = cache_for(client, config);
    let (runtime, test) = lock_parts(client, config)?;
    let mut classpath = Vec::new();
    for artifact in runtime.into_iter().chain(test) {
        classpath.push(cache.ensure_jar(&artifact, DEFAULT_REPO_URL)?);
    }
    Ok(classpath)
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
pub fn check_java_version(required: Option<&str>) -> anyhow::Result<()> {
    let Some(required) = required else {
        return Ok(());
    };
    let required_major = parse_major(required)
        .with_context(|| format!("invalid java version \"{required}\" in jip.toml"))?;
    let installed_major = java_major_version()?;
    if installed_major < required_major {
        bail!(
            "this project needs Java {required}, but the installed JDK is Java {installed_major} \
             (install a newer JDK and try again)"
        );
    }
    Ok(())
}

/// Query the major Java version from `java -version`.
pub fn java_major_version() -> anyhow::Result<u32> {
    let output = Command::new("java")
        .arg("-version")
        .output()
        .context("no `java` on PATH — install a JDK (e.g. via Homebrew or sdkman)")?;
    let stderr = String::from_utf8_lossy(&output.stderr);
    // Example lines: `openjdk version "21.0.2" ...` or `java version "1.8.0_392"`.
    let quoted = stderr
        .split('"')
        .nth(1)
        .context("cannot parse `java -version` output")?;
    parse_major(quoted)
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
