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
use crate::config::{CONFIG_FILE, ProjectConfig};
use crate::convert::{self, ConversionOffer};

/// Run the project's main class with all dependencies on the classpath.
pub fn run(
    client: &reqwest::blocking::Client,
    offline: bool,
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

    // Multi-module run: flatten all module classes + external deps.
    if crate::multi::is_multi_module(&config) {
        return run_multi_module(
            client,
            &mut config,
            offline,
            main_arg,
            defines,
            program_args,
        );
    }

    // Lazily download every jar that is not yet cached.
    let classpath = classpath_for(client, &config, offline)?;

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

    run_target(
        target,
        &classpath,
        client,
        &config,
        offline,
        defines,
        &program_args,
        false,
    )
}

/// Run a multi-module project: flatten all module classes onto the classpath.
fn run_multi_module(
    client: &reqwest::blocking::Client,
    config: &mut ProjectConfig,
    offline: bool,
    main_arg: Option<&str>,
    defines: &[String],
    program_args: &[String],
) -> anyhow::Result<()> {
    let layout = crate::multi::detect_multi_module()
        .context("multi-module config found but cannot detect module layout")?;

    let root_dir = std::env::current_dir()?;
    check_java_version(config.project.java.as_deref())?;

    // Build the classpath: all module target/classes + all external deps.
    // Sort modules so we build the classpath in dependency order.
    let sorted = crate::multi::topological_sort(&layout.modules)?;

    // Flatten: all modules' compiled classes (in topological order).
    let mut classpath = Vec::new();
    for module in &sorted {
        let classes = crate::multi::module_classes_dir(&root_dir, &module.path);
        if classes.exists() {
            classpath.push(classes);
        }
    }

    // Collect external dependency jars from each module's per-module config.
    let original_dir = std::env::current_dir()?;
    for module in &sorted {
        let module_config = crate::multi::load_module_config(&root_dir, &module.path)?;
        std::env::set_current_dir(root_dir.join(&module.path))?;
        let deps = compile_classpath_for(client, &module_config, offline)?;
        classpath.extend(deps);
    }
    std::env::set_current_dir(&original_dir)?;

    // Parse arguments.
    let mut program_args = program_args.to_vec();
    let main_arg = match main_arg {
        Some(arg) if !build::is_main_class(config, arg) => {
            program_args.insert(0, arg.to_string());
            None
        }
        arg => arg,
    };

    // Resolve main class — try root config first, then scan all modules.
    let target = match build::resolve_main(config, main_arg)? {
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
        MainDecision::None => {
            // Scan each module for a main class.
            let mut all_candidates = Vec::new();
            for module in &sorted {
                let module_config = crate::multi::load_module_config(&root_dir, &module.path)?;
                let original_dir = std::env::current_dir()?;
                std::env::set_current_dir(root_dir.join(&module.path))?;
                if let Ok(candidates) = build::main_candidates(&module_config) {
                    all_candidates.extend(candidates);
                }
                std::env::set_current_dir(&original_dir)?;
            }
            all_candidates.sort();
            all_candidates.dedup();

            if all_candidates.len() > 1 {
                bail!("{}", build::multiple_main_error(&all_candidates));
            }
            if let Some(fqcn) = all_candidates.into_iter().next() {
                MainTarget::Class(fqcn)
            } else {
                bail!(
                    "no main class found — write a class with `public static void main` \
                     in any module or set [project] main in jip.toml"
                );
            }
        }
    };

    run_target(
        target,
        &classpath,
        client,
        config,
        offline,
        defines,
        &program_args,
        true, // skip_compilation — modules are already built
    )
}

/// Execute a resolved main target with the given classpath.
#[allow(clippy::too_many_arguments)]
fn run_target(
    target: MainTarget,
    classpath: &[PathBuf],
    client: &reqwest::blocking::Client,
    config: &ProjectConfig,
    offline: bool,
    defines: &[String],
    program_args: &[String],
    already_compiled: bool,
) -> anyhow::Result<()> {
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
                .arg(classpath_string(classpath))
                .arg(&path);
        }
        MainTarget::Jar(path) => {
            if !path.exists() {
                bail!("cannot find {}", path.display());
            }
            let main_class = main_class_from_jar(&path)?;
            let mut full_classpath = classpath.to_vec();
            full_classpath.push(path);
            command
                .arg("--class-path")
                .arg(classpath_string(&full_classpath))
                .arg(&main_class);
        }
        MainTarget::Class(name) => {
            if !already_compiled {
                let compile_classpath = compile_classpath_for(client, config, offline)?;
                build::compile(config, &compile_classpath)?;
            }
            let mut run_classpath = vec![PathBuf::from(build::CLASSES_DIR)];
            run_classpath.extend(classpath.iter().cloned());
            command
                .arg("--class-path")
                .arg(classpath_string(&run_classpath))
                .arg(&name);
        }
    }
    command.args(program_args);

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
