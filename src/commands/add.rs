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
//  jip — `jip add`
//  ---------------------------------------------------------------------------
//  Adds a dependency to `jip.toml`, resolves the full dependency graph and
//  writes the updated `jip.lock`.
//
//  The argument is either `group:artifact:version` or `group:artifact`,
//  in which case the latest version on Maven Central is looked up.
//
//  Project:   jip
//  Author:    autumo GmbH
//  Date:      2026-08-15
// =============================================================================

use std::collections::BTreeMap;
use std::path::Path;

use anyhow::{Context, bail};

use crate::central;
use crate::commands::{
    repositories_for, require_config, resolve, resolve_provided, resolve_tests, write_lock,
};
use crate::config::CONFIG_FILE;
use crate::lock::LOCK_FILE;

/// Add a dependency and re-resolve.
pub fn run(
    client: &reqwest::blocking::Client,
    offline: bool,
    dependency: &str,
    test: bool,
    provided: bool,
) -> anyhow::Result<()> {
    let mut config = require_config()?;

    let (key, group, artifact) = parse_dependency_arg(dependency)?;
    let repos = repositories_for(&config);
    let (section, target): (&str, &mut BTreeMap<String, String>) = if provided {
        ("provided-dependencies", &mut config.provided_dependencies)
    } else if test {
        ("test-dependencies", &mut config.test_dependencies)
    } else {
        ("dependencies", &mut config.dependencies)
    };
    if target.contains_key(&key) {
        println!("{key} is already in [{section}] in {CONFIG_FILE}");
        return Ok(());
    }

    // Without an explicit version, ask the repositories for the latest one.
    let version = match parse_explicit_version(dependency) {
        Some(version) => version.to_string(),
        None => {
            let latest = central::latest_version(client, &repos, group, artifact)?
                .with_context(|| format!("no version found for {key} on Maven Central"))?;
            println!("latest version of {key} is {latest}");
            latest
        }
    };

    target.insert(key.clone(), version.clone());

    let resolution = resolve(client, &config, offline)?;
    let provided = resolve_provided(client, &config, offline)?;
    let tests = resolve_tests(client, &config, offline)?;
    write_lock(&resolution.flat, &provided.flat, &tests.flat)?;
    config.save(Path::new(CONFIG_FILE))?;

    println!(
        "{}",
        crate::console::green(&format!(
            "added {key}:{version} — {} packages in {LOCK_FILE}",
            resolution.flat.len()
        ))
    );
    Ok(())
}

/// Split `group:artifact[:version]` into its parts.
fn parse_dependency_arg(dependency: &str) -> anyhow::Result<(String, &str, &str)> {
    let mut parts = dependency.split(':');
    let group = parts.next().unwrap_or_default();
    let artifact = parts.next().unwrap_or_default();
    if group.is_empty() || artifact.is_empty() || parts.next().is_some_and(|v| v.is_empty()) {
        bail!("expected \"group:artifact\" or \"group:artifact:version\", got \"{dependency}\"");
    }
    Ok((format!("{group}:{artifact}"), group, artifact))
}

/// Return the version part when the argument is `group:artifact:version`.
fn parse_explicit_version(dependency: &str) -> Option<&str> {
    let mut parts = dependency.split(':');
    parts.next()?;
    parts.next()?;
    parts.next().filter(|v| !v.is_empty())
}
