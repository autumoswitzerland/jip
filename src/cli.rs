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

/// A pip-like dependency manager and runner for Java — clone it. Run it. Done.
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

    /// Package the project's compiled classes into a jar.
    ///
    /// Creates `target/app.jar` from the compiled classes in `target/classes`.
    /// With `--fat`, all dependency jars are merged into a single uber jar.
    /// After a thin jar is built, jip asks whether to add it to the
    /// runtime classpath.
    Jar {
        /// Build a fat (uber) jar that includes all dependencies.
        #[arg(long)]
        fat: bool,
    },

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
    ///
    /// Without an argument every direct dependency in `jip.toml` is checked
    /// and updated.  With a `group:artifact` key only that dependency is
    /// updated.
    Update {
        /// Dependency key `group:artifact` to update (default: all).
        dependency: Option<String>,
    },

    /// Show which dependencies have newer versions available (read-only).
    ///
    /// Prints `group:artifact: installed -> latest` for every direct
    /// dependency, without touching `jip.toml` or `jip.lock`.
    Outdated,

    /// List all resolved dependencies with their versions.
    ///
    /// Prints the runtime, compile-only (`provided`) and test dependencies
    /// from `jip.lock`, resolving first when the lock file is missing.
    List,

    /// Remove `target/` build artifacts.
    Clean,

    /// Print shell completions for bash, zsh, or fish.
    ///
    /// Direct the output to the shell's completion file, e.g.
    /// `jip completion zsh >> ~/.zshrc`.
    Completion {
        /// The shell to generate completions for: `bash`, `zsh` or `fish`.
        shell: String,
    },
}
