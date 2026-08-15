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

use crate::commands::{cache_for, ensure_jars, load_config, resolve, resolve_tests, write_lock};
use crate::lock::LOCK_FILE;

/// Resolve, download, and write the lock file.
pub fn run(client: &reqwest::blocking::Client) -> anyhow::Result<()> {
    let config = load_config()?;
    let resolution = resolve(client, &config)?;
    let tests = resolve_tests(client, &config)?;
    write_lock(&resolution.flat, &tests.flat)?;

    let cache = cache_for(client, &config);
    ensure_jars(&cache, &resolution.flat)?;
    ensure_jars(&cache, &tests.flat)?;

    let mut summary = format!("resolved {} packages", resolution.flat.len());
    if !tests.flat.is_empty() {
        summary.push_str(&format!(" and {} test packages", tests.flat.len()));
    }
    println!("{summary} — {LOCK_FILE} is up to date");
    Ok(())
}
