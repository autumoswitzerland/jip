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
//  jip — `jip java`
//  ---------------------------------------------------------------------------
//  Manages JDK installations: list, install, use, and remove.
//
//  Project:   jip
//  Author:    autumo GmbH
//  Date:      2026-08-18
// =============================================================================

use anyhow::bail;
use reqwest::blocking::Client;

use crate::jdk::{self, Vendor};

/// List installed JDKs.
pub fn list() -> anyhow::Result<()> {
    let installations = jdk::list_installed()?;

    if installations.is_empty() {
        println!("no JDKs installed — run `jip java install <version>` to get started");
        return Ok(());
    }

    println!("installed JDKs:");
    for jdk in &installations {
        let marker = if jdk.active { " *" } else { "" };
        println!("  {} {}{}", jdk.vendor, jdk.version, marker);
    }

    if installations.iter().any(|j| j.active) {
        println!("\n  * = active");
    } else {
        println!("\nno active JDK — run `jip java use <version>` to set one");
    }

    Ok(())
}

/// Install a JDK.
pub fn install(client: &Client, version: &str, vendor: Option<&str>) -> anyhow::Result<()> {
    let v = match vendor {
        Some(name) => Vendor::from_str(name)?,
        None => Vendor::Zulu,
    };

    jdk::install(client, v, version)?;
    Ok(())
}

/// Set the active JDK.
pub fn use_java(version: &str, vendor: Option<&str>) -> anyhow::Result<()> {
    let v = match vendor {
        Some(name) => Vendor::from_str(name)?,
        None => {
            let installations = jdk::list_installed()?;
            let matching: Vec<_> = installations
                .iter()
                .filter(|j| j.version == version)
                .collect();

            match matching.len() {
                0 => bail!("JDK {version} not installed — run `jip java install {version}` first"),
                1 => matching[0].vendor,
                _ => {
                    let vendors: Vec<String> =
                        matching.iter().map(|j| j.vendor.to_string()).collect();
                    bail!(
                        "JDK {version} is installed for multiple vendors: {} — specify --vendor",
                        vendors.join(", ")
                    );
                }
            }
        }
    };

    jdk::set_active(v, version)
}

/// Remove an installed JDK.
pub fn remove_java(version: &str, vendor: Option<&str>) -> anyhow::Result<()> {
    let v = match vendor {
        Some(name) => Vendor::from_str(name)?,
        None => {
            let installations = jdk::list_installed()?;
            let matching: Vec<_> = installations
                .iter()
                .filter(|j| j.version == version)
                .collect();

            match matching.len() {
                0 => bail!("JDK {version} not installed"),
                1 => matching[0].vendor,
                _ => {
                    let vendors: Vec<String> =
                        matching.iter().map(|j| j.vendor.to_string()).collect();
                    bail!(
                        "JDK {version} is installed for multiple vendors: {} — specify --vendor",
                        vendors.join(", ")
                    );
                }
            }
        }
    };

    jdk::remove(v, version)
}
