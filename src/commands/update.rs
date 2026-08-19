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
//  jip — `jip update`
//  ---------------------------------------------------------------------------
//  Checks every direct dependency (or a single one) for a newer version on
//  the repositories and updates `jip.toml` and `jip.lock` accordingly.
//
//  Project:   jip
//  Author:    autumo GmbH
//  Date:      2026-08-15
// =============================================================================

use std::collections::BTreeMap;
use std::io::{IsTerminal, Write};
use std::path::Path;

use anyhow::bail;

use crate::central;
use crate::commands::{
    repositories_for, require_config, resolve, resolve_provided, resolve_tests, write_lock,
};
use crate::config::CONFIG_FILE;

/// Update all direct dependencies — or a single one — to their latest
/// versions.
pub fn run(
    client: &reqwest::blocking::Client,
    offline: bool,
    dependency: Option<&str>,
) -> anyhow::Result<()> {
    let mut config = require_config()?;
    let repos = repositories_for(&config);

    let mut changed = 0;
    if let Some(key) = dependency {
        changed = update_one(client, &repos, key, &mut config)?;
    } else {
        changed += update_versions(client, &repos, &mut config.dependencies)?;
        changed += update_versions(client, &repos, &mut config.provided_dependencies)?;
        changed += update_versions(client, &repos, &mut config.test_dependencies)?;
    }

    if changed == 0 {
        println!(
            "{}",
            crate::console::green("all dependencies are up to date")
        );
        return Ok(());
    }

    if !std::io::stdin().is_terminal() {
        bail!("run `jip update` interactively to confirm version bumps");
    }

    print!(
        "\n{} ",
        crate::console::bold(&format!(
            "update {changed} dependencies to the versions listed above?"
        ))
    );
    print!("[y/N] ");
    std::io::stdout().flush()?;
    let mut answer = String::new();
    std::io::stdin().read_line(&mut answer)?;
    let answer = answer.trim();
    if !matches!(answer, "y" | "Y" | "yes" | "Yes") {
        println!("update cancelled — {} and jip.lock unchanged", CONFIG_FILE);
        return Ok(());
    }

    let resolution = resolve(client, &config, offline)?;
    let provided = resolve_provided(client, &config, offline)?;
    let tests = resolve_tests(client, &config, offline)?;
    write_lock(&resolution.flat, &provided.flat, &tests.flat)?;
    config.save(Path::new(CONFIG_FILE))?;
    println!(
        "{}",
        crate::console::green(&format!(
            "updated {changed} dependencies — {CONFIG_FILE} and jip.lock are in sync"
        ))
    );
    Ok(())
}

/// Update a single dependency across the runtime, compile-only and test
/// sections, returning how many entries changed.
fn update_one(
    client: &reqwest::blocking::Client,
    repos: &[String],
    key: &str,
    config: &mut crate::config::ProjectConfig,
) -> anyhow::Result<usize> {
    if let Some(version) = config.dependencies.get(key) {
        update_dep(
            client,
            repos,
            key,
            &version.clone(),
            &mut config.dependencies,
        )
    } else if let Some(version) = config.provided_dependencies.get(key) {
        update_dep(
            client,
            repos,
            key,
            &version.clone(),
            &mut config.provided_dependencies,
        )
    } else if let Some(version) = config.test_dependencies.get(key) {
        update_dep(
            client,
            repos,
            key,
            &version.clone(),
            &mut config.test_dependencies,
        )
    } else {
        bail!(
            "{key} is not a direct dependency in {CONFIG_FILE} — \
             use the key from `jip list`"
        );
    }
}

/// Look up the newest version for one dependency key and note the bump.
fn update_dep(
    client: &reqwest::blocking::Client,
    repos: &[String],
    key: &str,
    version: &str,
    deps: &mut BTreeMap<String, String>,
) -> anyhow::Result<usize> {
    let Some((group, artifact)) = key.split_once(':') else {
        bail!("expected \"group:artifact\", got \"{key}\"");
    };
    let Some(latest) = central::latest_version(client, repos, group, artifact)? else {
        bail!("no newer version found for {key} — is it on a configured repository?");
    };
    if latest == *version {
        println!("{key}: already at {version}");
        return Ok(0);
    }
    println!("{key}: {version} -> {latest}");
    deps.insert(key.to_string(), latest);
    Ok(1)
}

/// Update one dependency map to the latest versions on the repositories,
/// returning how many entries changed.
fn update_versions(
    client: &reqwest::blocking::Client,
    repos: &[String],
    deps: &mut BTreeMap<String, String>,
) -> anyhow::Result<usize> {
    let mut changed = 0;
    for (key, version) in deps.iter_mut() {
        let Some((group, artifact)) = key.split_once(':') else {
            continue;
        };
        let Some(latest) = central::latest_version(client, repos, group, artifact)? else {
            println!("{key}: no version found on Maven Central");
            continue;
        };
        if latest == *version {
            println!("{key}: already at {version}");
        } else {
            println!("{key}: {version} -> {latest}");
            *version = latest;
            changed += 1;
        }
    }
    Ok(changed)
}
