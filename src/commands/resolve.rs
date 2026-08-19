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
//  jip — `jip resolve`
//  ---------------------------------------------------------------------------
//  Resolves all dependencies, downloads any missing jars, and writes
//  `jip.lock`.  Useful when a checkout was updated and the lock file or
//  the local cache is out of date.
//
//  Project:   jip
//  Author:    autumo GmbH
//  Date:      2026-08-15
// =============================================================================

use crate::commands::{
    cache_for, ensure_jars, load_config, repositories_for, resolve, resolve_provided,
    resolve_tests, write_lock,
};
use crate::lock::LOCK_FILE;

/// Resolve, download, and write the lock file.
pub fn run(client: &reqwest::blocking::Client, offline: bool) -> anyhow::Result<()> {
    let config = load_config()?;
    let resolution = resolve(client, &config, offline)?;
    let provided = resolve_provided(client, &config, offline)?;
    let tests = resolve_tests(client, &config, offline)?;
    write_lock(&resolution.flat, &provided.flat, &tests.flat)?;

    let repos = repositories_for(&config);
    let cache = cache_for(client, &config, offline);
    ensure_jars(&cache, &resolution.flat, &repos)?;
    ensure_jars(&cache, &provided.flat, &repos)?;
    ensure_jars(&cache, &tests.flat, &repos)?;

    let mut summary = format!("resolved {} packages", resolution.flat.len());
    if !provided.flat.is_empty() {
        summary.push_str(&format!(" and {} provided packages", provided.flat.len()));
    }
    if !tests.flat.is_empty() {
        summary.push_str(&format!(" and {} test packages", tests.flat.len()));
    }
    println!(
        "{}",
        crate::console::green(&format!("{summary} — {LOCK_FILE} is up to date"))
    );
    Ok(())
}
