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
//  jip — `jip remove`
//  ---------------------------------------------------------------------------
//  Removes a dependency from `jip.toml` and writes an updated `jip.lock`.
//
//  Project:   jip
//  Author:    autumo GmbH
//  Date:      2026-08-15
// =============================================================================

use std::path::Path;

use anyhow::bail;

use crate::commands::{require_config, resolve, resolve_tests, write_lock};
use crate::config::CONFIG_FILE;

/// Remove a dependency and re-resolve.
pub fn run(client: &reqwest::blocking::Client, dependency: &str, test: bool) -> anyhow::Result<()> {
    let mut config = require_config()?;

    let section = if test {
        "test-dependencies"
    } else {
        "dependencies"
    };
    let removed = if test {
        config.test_dependencies.remove(dependency)
    } else {
        config.dependencies.remove(dependency)
    };
    if removed.is_none() {
        bail!("{dependency} is not in [{section}] in {CONFIG_FILE}");
    }

    let resolution = resolve(client, &config)?;
    let tests = resolve_tests(client, &config)?;
    write_lock(&resolution.flat, &tests.flat)?;
    config.save(Path::new(CONFIG_FILE))?;

    println!(
        "removed {dependency} — {} packages in jip.lock",
        resolution.flat.len()
    );
    Ok(())
}
