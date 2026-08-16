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
//  jip — Command Line Interface
//  ---------------------------------------------------------------------------
//  Defines every jip subcommand and its arguments using clap's derive API.
//  Each subcommand maps to one module in `commands/`.
//
//  Project:   jip
//  Author:    autumo GmbH
//  Date:      2026-08-15
// =============================================================================

use clap::{Parser, Subcommand};

/// A pip-like dependency manager and runner for Java.
#[derive(Debug, Parser)]
#[command(name = "jip", version, about, long_about = None)]
pub struct Jip {
    #[command(subcommand)]
    pub command: Command,
}

/// All subcommands jip understands.
#[derive(Debug, Subcommand)]
pub enum Command {
    /// Create a new `jip.toml` in the current directory.
    Init,

    /// Add a dependency to `jip.toml` and resolve it.
    ///
    /// The dependency is given as `group:artifact:version`.  When the version
    /// is omitted, the latest version is looked up automatically.
    Add {
        /// Dependency in the form `group:artifact:version` (version optional).
        dependency: String,
        /// Add to `[test-dependencies]` instead of `[dependencies]`.
        #[arg(long)]
        test: bool,
        /// Add to `[provided-dependencies]` (compile-only) instead of `[dependencies]`.
        #[arg(long, conflicts_with = "test")]
        provided: bool,
    },

    /// Remove a dependency from `jip.toml`.
    Remove {
        /// Dependency key in the form `group:artifact`.
        dependency: String,
        /// Remove from `[test-dependencies]` instead of `[dependencies]`.
        #[arg(long)]
        test: bool,
        /// Remove from `[provided-dependencies]` (compile-only) instead of `[dependencies]`.
        #[arg(long, conflicts_with = "test")]
        provided: bool,
    },

    /// Resolve all dependencies, download missing jars, and write `jip.lock`.
    Resolve,

    /// Compile the project's sources into `target/classes`.
    ///
    /// Recompilation is skipped while the sources are up to date, so `jip run`
    /// can rely on this being cheap.
    Build,

    /// Compile and run the project's JUnit tests.
    ///
    /// Tests in `src/test/java` are compiled against the main classes and
    /// dependencies, then executed with the JUnit Platform Console Launcher
    /// from the `junit-platform-console-standalone` dependency.  With no
    /// test sources the command reports nothing to do.
    Test,

    /// Run the project's main class with all dependencies on the classpath.
    ///
    /// Accepts a fully qualified class name (compiled first if needed), a
    /// `.java` file (started directly), or a `.jar`.  Without an argument,
    /// the `main` value from `jip.toml` is used, or a class with a
    /// `public static void main` method is detected automatically.
    ///
    /// Everything else is passed to the program: `jip run start` runs
    /// `[project] main` with `start` as its first argument.  Use `--` before
    /// arguments that start with a hyphen: `jip run -- -h`.
    Run {
        /// Main class or file (.java/.jar).  Defaults to `main` in
        /// `jip.toml`, or auto-detection.
        main: Option<String>,

        /// JVM system property, `NAME=VALUE` (repeatable, passed to `java`).
        #[arg(short = 'D', long = "define", value_name = "NAME=VALUE")]
        defines: Vec<String>,

        /// Arguments passed through to the program.
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },

    /// Search for libraries on Maven Central.
    Search {
        /// Free-text search query.
        query: String,
    },

    /// Show the resolved dependency tree.
    Tree,

    /// Update direct dependencies to their latest versions and re-resolve.
    Update,
}
