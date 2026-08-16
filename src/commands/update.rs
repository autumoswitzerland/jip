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
//  Checks every direct dependency for a newer version on Maven Central and
//  updates `jip.toml` and `jip.lock` accordingly.
//
//  Project:   jip
//  Author:    autumo GmbH
//  Date:      2026-08-15
// =============================================================================

use std::collections::BTreeMap;
use std::path::Path;

use crate::central;
use crate::commands::{
    repositories_for, require_config, resolve, resolve_provided, resolve_tests, write_lock,
};
use crate::config::CONFIG_FILE;

/// Update all direct dependencies to their latest versions.
pub fn run(client: &reqwest::blocking::Client) -> anyhow::Result<()> {
    let mut config = require_config()?;
    let repos = repositories_for(&config);

    let mut changed = 0;
    changed += update_versions(client, &repos, &mut config.dependencies)?;
    changed += update_versions(client, &repos, &mut config.provided_dependencies)?;
    changed += update_versions(client, &repos, &mut config.test_dependencies)?;

    if changed == 0 {
        println!(
            "{}",
            crate::console::green("all dependencies are up to date")
        );
        return Ok(());
    }

    let resolution = resolve(client, &config)?;
    let provided = resolve_provided(client, &config)?;
    let tests = resolve_tests(client, &config)?;
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
