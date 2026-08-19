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
//  jip — `jip tree`
//  ---------------------------------------------------------------------------
//  Prints the resolved dependency tree, with every winning version.
//
//  Project:   jip
//  Author:    autumo GmbH
//  Date:      2026-08-15
// =============================================================================

use crate::commands::{load_config, resolve, resolve_tests};
use crate::resolver::DependencyNode;

/// Resolve and print the dependency tree.
pub fn run(client: &reqwest::blocking::Client, offline: bool) -> anyhow::Result<()> {
    let config = load_config()?;
    let resolution = resolve(client, &config, offline)?;
    let tests = resolve_tests(client, &config, offline)?;

    if resolution.tree.is_empty() && tests.tree.is_empty() {
        println!("no dependencies");
        return Ok(());
    }

    for node in &resolution.tree {
        print_node(node, 0);
    }
    if !tests.tree.is_empty() {
        println!("test dependencies:");
        for node in &tests.tree {
            print_node(node, 1);
        }
    }
    Ok(())
}

/// Print a node and all its children, indented by depth.
fn print_node(node: &DependencyNode, depth: usize) {
    println!("{}{}", "  ".repeat(depth), node.artifact);
    for child in &node.children {
        print_node(child, depth + 1);
    }
}
