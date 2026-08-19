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
//  jip — `jip info`
//  ---------------------------------------------------------------------------
//  Shows metadata for a dependency: latest version, description, and license.
//
//  Project:   jip
//  Author:    autumo GmbH
//  Date:      2026-08-19
// =============================================================================

use anyhow::Context;
use roxmltree::Document;

use crate::cache::download_repo_text;
use crate::central;
use crate::commands::{load_config, repositories_for};

/// Show metadata for a dependency.
pub fn run(client: &reqwest::blocking::Client, dependency: &str) -> anyhow::Result<()> {
    let (group, artifact, explicit_version) = parse_coordinates(dependency)?;

    let config = load_config().ok();
    let repos = config.as_ref().map(repositories_for).unwrap_or_default();

    // Determine version
    let version = if let Some(v) = explicit_version {
        v.to_string()
    } else {
        central::latest_version(client, &repos, group, artifact)?
            .with_context(|| format!("no version found for {group}:{artifact}"))?
    };

    println!("group:    {group}");
    println!("artifact: {artifact}");
    println!("version:  {version}");

    // Download the POM to get description and license
    let pom_path = format!(
        "{}/{}/{version}/{artifact}-{version}.pom",
        group.replace('.', "/"),
        artifact
    );
    for repo in &repos {
        if let Ok(xml) = download_repo_text(client, repo, &pom_path) {
            if let Ok(doc) = Document::parse(&xml) {
                let root = doc.root_element();

                if let Some(desc) = text_of(root, "description") {
                    let desc = desc.trim();
                    if !desc.is_empty() {
                        println!("description:");
                        for line in desc.lines() {
                            println!("  {}", line.trim());
                        }
                    }
                }

                if let Some(name) = text_of(root, "name") {
                    let name = name.trim();
                    if !name.is_empty() && name != artifact {
                        println!("name:     {name}");
                    }
                }

                if let Some(url) = text_of(root, "url") {
                    let url = url.trim();
                    if !url.is_empty() {
                        println!("url:      {url}");
                    }
                }

                // License from <licenses><license><name>
                if let Some(licenses) = child(root, "licenses") {
                    for license_node in children(licenses, "license") {
                        if let Some(name) = text_of(license_node, "name") {
                            let name = name.trim();
                            if !name.is_empty() {
                                println!("license:  {name}");
                            }
                        }
                    }
                }
            }
            return Ok(());
        }
    }

    // No POM found — that's ok, we still have version info
    Ok(())
}

/// Parse `group:artifact[:version]` into its parts.
fn parse_coordinates(input: &str) -> anyhow::Result<(&str, &str, Option<&str>)> {
    let mut parts = input.splitn(3, ':');
    let group = parts
        .next()
        .filter(|s| !s.is_empty())
        .context("missing group in coordinates")?;
    let artifact = parts
        .next()
        .filter(|s| !s.is_empty())
        .context("missing artifact in coordinates — use group:artifact")?;
    let version = parts.next().filter(|s| !s.is_empty());
    Ok((group, artifact, version))
}

// --- Minimal POM helpers (only what info needs) ---

fn text_of<'a>(node: roxmltree::Node<'a, 'a>, tag: &str) -> Option<&'a str> {
    child(node, tag).and_then(|child| child.text())
}

fn child<'a>(node: roxmltree::Node<'a, 'a>, tag: &str) -> Option<roxmltree::Node<'a, 'a>> {
    node.children().find(|c| c.has_tag_name(tag))
}

fn children<'a>(node: roxmltree::Node<'a, 'a>, tag: &str) -> Vec<roxmltree::Node<'a, 'a>> {
    node.children().filter(|c| c.has_tag_name(tag)).collect()
}
