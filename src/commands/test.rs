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
//  jip — `jip test`
//  ---------------------------------------------------------------------------
//  Compiles the tests from `src/test/java` (Maven layout) against the main
//  classes and dependencies, then runs them with the JUnit Platform Console
//  Launcher.  The launcher comes from the single shaded
//  `junit-platform-console-standalone` jar, which bundles the launcher,
//  engine, and jupiter API, so one `jip add` is all a project needs.
//
//  Project:   jip
//  Author:    autumo GmbH
//  Date:      2026-08-15
// =============================================================================

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::Context;

use crate::commands::build::{self, CLASSES_DIR};
use crate::commands::{
    check_java_version, classpath_string, compile_classpath_for, load_config,
    provided_classpath_for, test_classpath_for,
};
use crate::convert::{self, ConversionOffer};

/// Default test source directory (the Maven layout).
pub const TEST_SOURCE_DIR: &str = "src/test/java";
/// Directory the compiled test `.class` files land in.
pub const TEST_CLASSES_DIR: &str = "target/test-classes";

/// Compile and run the project's JUnit tests.
pub fn run(client: &reqwest::blocking::Client) -> anyhow::Result<()> {
    // Offer to convert a detected Maven/Gradle project when jip.toml is
    // missing, just like `jip build` and `jip run`.
    let config = match convert::offer_conversion(client)? {
        ConversionOffer::Converted(config) => config,
        ConversionOffer::Declined => return Ok(()),
        ConversionOffer::Proceed => Box::new(load_config()?),
    };

    let test_dir = Path::new(TEST_SOURCE_DIR);
    let sources = build::collect_java_files(test_dir);
    if sources.is_empty() {
        println!(
            "no test sources in {} — nothing to test",
            test_dir.display()
        );
        return Ok(());
    }

    // Lazily download every jar that is not yet cached.
    let test_classpath = test_classpath_for(client, &config)?;
    let standalone = test_classpath
        .iter()
        .find(|path| is_console_standalone(path))
        .with_context(|| {
            "no junit-platform-console-standalone on the test classpath — run \
             `jip add org.junit.platform:junit-platform-console-standalone --test`"
        })?;
    check_java_version(config.project.java.as_deref())?;

    // The main classes compile against runtime plus provided dependencies;
    // the tests additionally see the test dependencies.
    let compile_classpath = compile_classpath_for(client, &config)?;
    build::compile(&config, &compile_classpath)?;

    let provided = provided_classpath_for(client, &config)?;
    let mut test_compile_classpath = vec![PathBuf::from(CLASSES_DIR)];
    test_compile_classpath.extend(provided);
    test_compile_classpath.extend(test_classpath.iter().cloned());
    let test_classes = Path::new(TEST_CLASSES_DIR);
    build::compile_java(
        &sources,
        &test_compile_classpath,
        test_classes,
        &[PathBuf::from(CLASSES_DIR)],
    )?;

    let mut run_classpath = vec![PathBuf::from(CLASSES_DIR), test_classes.to_path_buf()];
    run_classpath.extend(test_classpath.iter().cloned());

    // Run the launcher with the test classes on the scan path, keeping the
    // child's output and exit code.  JUnit Platform 6+ moved the options
    // behind an `execute` subcommand.
    let java = crate::commands::java_binary()?;
    let mut command = Command::new(&java);
    command
        .arg("--class-path")
        .arg(classpath_string(&run_classpath))
        .arg("org.junit.platform.console.ConsoleLauncher");
    if standalone_version(standalone).is_some_and(|v| platform_major(&v) >= 6) {
        command.arg("execute");
    }
    command
        .arg("--class-path")
        .arg(test_classes)
        .arg("--scan-class-path")
        .arg("--details")
        .arg("tree");
    let status = command
        .status()
        .with_context(|| format!("failed to start {} — is a JDK installed?", java.display()))?;
    if !status.success() {
        std::process::exit(status.code().unwrap_or(1));
    }
    Ok(())
}

/// Is this classpath entry the shaded JUnit console launcher jar?
fn is_console_standalone(path: &Path) -> bool {
    path.file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.starts_with("junit-platform-console-standalone"))
}

/// The platform version encoded in the standalone jar's file name.
fn standalone_version(path: &Path) -> Option<String> {
    let name = path.file_name()?.to_str()?;
    name.strip_prefix("junit-platform-console-standalone-")?
        .strip_suffix(".jar")
        .map(str::to_string)
}

/// The major platform version, e.g. `6.1.3` -> 6.
fn platform_major(version: &str) -> u32 {
    version
        .split(['.', '-', '_'])
        .next()
        .and_then(|v| v.parse().ok())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognises_console_standalone_jar() {
        let standalone = Path::new(
            "/tmp/jip-cache/org.junit.platform/junit-platform-console-standalone-1.11.0.jar",
        );
        assert!(is_console_standalone(standalone));
        assert!(!is_console_standalone(Path::new(
            "/tmp/jip-cache/junit-jupiter-api-5.11.0.jar"
        )));
        assert!(!is_console_standalone(Path::new(
            "/tmp/jip-cache/junit-platform-console-1.11.0.jar"
        )));
    }

    #[test]
    fn extracts_platform_version_from_jar_name() {
        assert_eq!(
            standalone_version(Path::new(
                "/tmp/jip-cache/junit-platform-console-standalone-6.1.3.jar"
            ))
            .as_deref(),
            Some("6.1.3")
        );
        assert_eq!(
            standalone_version(Path::new(
                "/tmp/jip-cache/junit-platform-console-standalone-1.13.0-M3.jar"
            ))
            .as_deref(),
            Some("1.13.0-M3")
        );
        assert_eq!(standalone_version(Path::new("guava.jar")), None);
    }

    #[test]
    fn platform_six_requires_execute_subcommand() {
        assert_eq!(platform_major("6.1.3"), 6);
        assert_eq!(platform_major("1.13.0-M3"), 1);
        assert_eq!(platform_major("1.8.0"), 1);
        assert_eq!(platform_major("garbage"), 0);
    }
}
