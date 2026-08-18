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
//  jip — `jip outdated`
//  ---------------------------------------------------------------------------
//  Reports which direct dependencies have a newer version on their
//  repositories.  Read-only: `jip.toml` and `jip.lock` are never touched.
//
//  Project:   jip
//  Author:    autumo GmbH
//  Date:      2026-08-17
// =============================================================================

use std::collections::BTreeMap;

use crate::central;
use crate::commands::{repositories_for, require_config};

/// Check every direct dependency for a newer version and print the results.
pub fn run(client: &reqwest::blocking::Client) -> anyhow::Result<()> {
    let config = require_config()?;
    let repos = repositories_for(&config);

    let mut outdated = Vec::new();
    collect(client, &repos, &config.dependencies, &mut outdated)?;
    collect(client, &repos, &config.provided_dependencies, &mut outdated)?;
    collect(client, &repos, &config.test_dependencies, &mut outdated)?;

    if outdated.is_empty() {
        println!(
            "{}",
            crate::console::green("all dependencies are up to date")
        );
        return Ok(());
    }

    for (key, installed, latest) in &outdated {
        println!("{key}: {installed} -> {latest}");
    }
    println!(
        "\n{}",
        crate::console::bold(&format!(
            "{} dependency(ies) can be updated — run `jip update` or `jip update <group:artifact>`",
            outdated.len()
        ))
    );
    Ok(())
}

/// Look up the newest version of every entry in `deps` and collect the ones
/// that are behind.
fn collect(
    client: &reqwest::blocking::Client,
    repos: &[String],
    deps: &BTreeMap<String, String>,
    outdated: &mut Vec<(String, String, String)>,
) -> anyhow::Result<()> {
    for (key, version) in deps {
        let Some((group, artifact)) = key.split_once(':') else {
            continue;
        };
        if let Some(latest) = central::latest_version(client, repos, group, artifact)?
            && latest != *version
        {
            outdated.push((key.clone(), version.clone(), latest));
        }
    }
    Ok(())
}
