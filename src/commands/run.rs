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
//  jip — `jip run`
//  ---------------------------------------------------------------------------
//  The flagship command.  Makes sure every dependency is available, builds
//  the classpath from `jip.lock`, and starts the program with the system
//  JDK.  Runs a single `.java` file directly, starts `.jar` files via their
//  `Main-Class`, and compiles multi-file projects first when needed.
//
//  This is what makes "clone a project, run it" possible.
//
//  Project:   jip
//  Author:    autumo GmbH
//  Date:      2026-08-15
// =============================================================================

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, bail};

use crate::commands::build::{self, MainDecision, MainTarget};
use crate::commands::{
    check_java_version, classpath_for, classpath_string, compile_classpath_for, load_config,
};
use crate::config::CONFIG_FILE;
use crate::convert::{self, ConversionOffer};

/// Run the project's main class with all dependencies on the classpath.
pub fn run(
    client: &reqwest::blocking::Client,
    main_arg: Option<&str>,
    defines: &[String],
    program_args: &[String],
) -> anyhow::Result<()> {
    // Offer to convert a detected Maven/Gradle project when jip.toml is
    // missing, so `jip run` works right after cloning a foreign repo.
    let mut config = match convert::offer_conversion(client)? {
        ConversionOffer::Converted(config) => config,
        ConversionOffer::Declined => return Ok(()),
        ConversionOffer::Proceed => Box::new(load_config()?),
    };

    // Lazily download every jar that is not yet cached.
    let classpath = classpath_for(client, &config)?;

    check_java_version(config.project.java.as_deref())?;

    // A positional that is not a main class of this project is the first
    // program argument, so `jip run start` runs `[project] main` with
    // `start` instead of failing to load a class named `start`.
    let mut program_args = program_args.to_vec();
    let main_arg = match main_arg {
        Some(arg) if !build::is_main_class(&config, arg) => {
            program_args.insert(0, arg.to_string());
            None
        }
        arg => arg,
    };

    let target = match build::resolve_main(&config, main_arg)? {
        MainDecision::Run(target) => target,
        MainDecision::Multiple(candidates) => {
            let target = build::choose_main(&candidates)?;
            if let MainTarget::Class(fqcn) = &target {
                config.project.main = Some(fqcn.clone());
                config.save(Path::new(CONFIG_FILE))?;
                println!(
                    "{}",
                    crate::console::green(&format!("saved [project] main = \"{fqcn}\""))
                );
            }
            target
        }
        MainDecision::None => bail!(
            "no main class found — write a class with `public static void main` \
             under {} or set [project] main in jip.toml",
            build::source_dir(&config).display()
        ),
    };

    let java = crate::commands::java_binary()?;
    let mut command = Command::new(&java);
    for define in defines {
        command.arg(format!("-D{define}"));
    }
    match target {
        MainTarget::SourceFile(path) => {
            if !path.exists() {
                bail!("cannot find {}", path.display());
            }
            command
                .arg("--class-path")
                .arg(classpath_string(&classpath))
                .arg(&path);
        }
        MainTarget::Jar(path) => {
            if !path.exists() {
                bail!("cannot find {}", path.display());
            }
            let main_class = main_class_from_jar(&path)?;
            let mut classpath = classpath;
            classpath.push(path);
            command
                .arg("--class-path")
                .arg(classpath_string(&classpath))
                .arg(&main_class);
        }
        MainTarget::Class(name) => {
            // Provided dependencies are compile-time only: javac sees them,
            // the running program does not.
            let compile_classpath = compile_classpath_for(client, &config)?;
            build::compile(&config, &compile_classpath)?;
            let mut run_classpath = vec![PathBuf::from(build::CLASSES_DIR)];
            run_classpath.extend(classpath);
            command
                .arg("--class-path")
                .arg(classpath_string(&run_classpath))
                .arg(&name);
        }
    }
    command.args(&program_args);

    // Hand over to the child process: same output, same exit code.
    let status = command
        .status()
        .with_context(|| format!("failed to start {} — is a JDK installed?", java.display()))?;
    if !status.success() {
        std::process::exit(status.code().unwrap_or(1));
    }
    Ok(())
}

/// Read the `Main-Class` entry from a jar's manifest.
fn main_class_from_jar(path: &Path) -> anyhow::Result<String> {
    let file =
        std::fs::File::open(path).with_context(|| format!("cannot open {}", path.display()))?;
    let mut archive = zip::ZipArchive::new(file).context("invalid jar file")?;
    let mut manifest = archive
        .by_name("META-INF/MANIFEST.MF")
        .with_context(|| format!("{} has no META-INF/MANIFEST.MF", path.display()))?;
    let mut content = String::new();
    std::io::Read::read_to_string(&mut manifest, &mut content)?;

    for line in content.lines() {
        if let Some(value) = line.trim().strip_prefix("Main-Class:") {
            let main_class = value.trim();
            if !main_class.is_empty() {
                return Ok(main_class.to_string());
            }
        }
    }
    bail!("no Main-Class entry found in {}", path.display())
}
