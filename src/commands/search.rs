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
//  jip — `jip search`
//  ---------------------------------------------------------------------------
//  Searches Maven Central and prints matching artifacts.
//
//  Project:   jip
//  Author:    autumo GmbH
//  Date:      2026-08-15
// =============================================================================

use crate::central;

const RESULT_LIMIT: u32 = 20;

/// Search Maven Central and print the results.
pub fn run(client: &reqwest::blocking::Client, query: &str) -> anyhow::Result<()> {
    let results = central::search(client, query, RESULT_LIMIT)?;

    if results.is_empty() {
        println!("no results for \"{query}\"");
        return Ok(());
    }

    for result in results {
        println!(
            "{}:{}  (latest {})",
            result.group, result.artifact, result.latest_version
        );
    }
    Ok(())
}
