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
//  jip — `jip build`
//  ---------------------------------------------------------------------------
//  Compiles every `.java` file under the source directory into
//  `target/classes` using the system `javac`.  Recompilation is skipped
//  while the sources (and the project/lock files) are older than the
//  compiled output, so `jip run` can rely on this being cheap.
//
//  Also owns the main-class detection: the entry point for `jip run` is
//  either given explicitly (a class name or a `.java`/`.jar` file) or found
//  by scanning for a class with a `public static void main` method.
//
//  Project:   jip
//  Author:    autumo GmbH
//  Date:      2026-08-15
// =============================================================================

use std::fs;
use std::io::{IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;

use anyhow::{Context, bail};
use regex::Regex;

use crate::commands::{check_java_version, classpath_string, compile_classpath_for, load_config};
use crate::config::ProjectConfig;
use crate::convert::{self, ConversionOffer};

/// Default source directory (the Maven/Gradle layout everyone knows).
pub const DEFAULT_SOURCE_DIR: &str = "src/main/java";
/// Directory the compiled `.class` files land in.
pub const CLASSES_DIR: &str = "target/classes";

/// What `jip run` should start: a source file, a jar, or a class on the
/// compiled classpath.
pub enum MainTarget {
    /// A single `.java` file, started directly by the JDK (no build step).
    SourceFile(PathBuf),
    /// A pre-built `.jar`, started via its `Main-Class` manifest entry.
    Jar(PathBuf),
    /// A fully qualified class name, compiled first if needed.
    Class(String),
}

/// The outcome of resolving what `jip run` should start.
pub enum MainDecision {
    /// A concrete run target was found.
    Run(MainTarget),
    /// Several classes with a `main` method exist; the user should pick one.
    Multiple(Vec<String>),
    /// Nothing runnable could be found.
    None,
}

/// The `jip build` command: compile everything, unless already up to date.
pub fn run(client: &reqwest::blocking::Client, offline: bool) -> anyhow::Result<()> {
    let config = match convert::offer_conversion(client)? {
        ConversionOffer::Converted(config) => config,
        ConversionOffer::Declined => return Ok(()),
        ConversionOffer::Proceed => Box::new(load_config()?),
    };

    // Multi-module build: compile each module in topological order.
    if crate::multi::is_multi_module(&config) {
        return run_multi_module(client, &config, offline);
    }

    let classpath = compile_classpath_for(client, &config, offline)?;
    check_java_version(config.project.java.as_deref())?;
    compile(&config, &classpath)
}

/// Build all modules in topological order.
pub(crate) fn run_multi_module(
    client: &reqwest::blocking::Client,
    root_config: &ProjectConfig,
    offline: bool,
) -> anyhow::Result<()> {
    let layout = crate::multi::detect_multi_module()
        .context("multi-module config found but cannot detect module layout")?;

    let sorted = crate::multi::topological_sort(&layout.modules)?;
    let root_dir = std::env::current_dir()?;

    check_java_version(root_config.project.java.as_deref())?;

    println!(
        "{}",
        crate::console::green(&format!(
            "building {} modules in order: {}",
            sorted.len(),
            sorted
                .iter()
                .map(|m| m.name.as_str())
                .collect::<Vec<_>>()
                .join(" -> ")
        ))
    );

    for module in &sorted {
        let module_config = crate::multi::load_module_config(&root_dir, &module.path)?;

        // cd into the module directory so lock/cache resolution finds the
        // per-module jip.lock and jip.toml (they use CWD-relative paths).
        let original_dir = std::env::current_dir()?;
        std::env::set_current_dir(root_dir.join(&module.path))?;

        // Aggregator/BOM/parent modules legitimately have no sources — skip
        // them instead of failing the whole build.
        if !has_sources(&module_config) {
            let reason = if has_kotlin_sources(&module_config) {
                " (Kotlin sources are not supported — jip compiles Java only)"
            } else {
                ""
            };
            println!(
                "{}",
                crate::console::yellow(&format!(
                    "skipping module '{}' — no .java sources under {}{reason}",
                    module.name,
                    source_dir(&module_config).display()
                ))
            );
            std::env::set_current_dir(&original_dir)?;
            continue;
        }

        // Build the classpath: external deps + inter-module target/classes.
        let external_classpath = compile_classpath_for(client, &module_config, offline)?;
        let full_classpath =
            crate::multi::module_classpath(&root_dir, &layout, module, &external_classpath);

        println!(
            "{}",
            crate::console::green(&format!("building module '{}'...", module.name))
        );

        let result = compile(&module_config, &full_classpath);

        std::env::set_current_dir(&original_dir)?;

        result?;
    }

    Ok(())
}

/// Compile the project's sources into `target/classes`, skipping the work
/// when the output is already newer than every source and marker file.
pub fn compile(config: &ProjectConfig, classpath: &[PathBuf]) -> anyhow::Result<()> {
    let source_dir = source_dir(config);
    let sources = collect_java_files(&source_dir);
    if sources.is_empty() {
        if has_kotlin_sources(config) {
            bail!(
                "no .java files found in {} — jip compiles Java only; \
                 Kotlin sources under {} are not supported",
                source_dir.display(),
                source_dir.with_file_name("kotlin").display()
            );
        }
        bail!(
            "no .java files found in {} — put your sources there",
            source_dir.display()
        );
    }

    compile_java(&sources, classpath, Path::new(CLASSES_DIR), &[])
}

/// Compile `.java` files into `out_dir`, skipping the work while the output
/// is newer than every source, every `extra_inputs` path, and the project
/// markers.  `extra_inputs` are freshness inputs the output depends on, such
/// as the main classes when compiling tests.
pub fn compile_java(
    sources: &[PathBuf],
    classpath: &[PathBuf],
    out_dir: &Path,
    extra_inputs: &[PathBuf],
) -> anyhow::Result<()> {
    if is_up_to_date(sources, out_dir, extra_inputs)? {
        println!(
            "{}",
            crate::console::green(&format!(
                "up to date — {} source files in {}",
                sources.len(),
                out_dir.display()
            ))
        );
        return Ok(());
    }

    fs::create_dir_all(out_dir).with_context(|| format!("cannot create {}", out_dir.display()))?;
    let javac = crate::commands::javac_binary()?;
    let status = Command::new(&javac)
        .arg("-d")
        .arg(out_dir)
        .arg("-classpath")
        .arg(classpath_string(classpath))
        .args(sources)
        .status()
        .with_context(|| format!("failed to start {} — is a JDK installed?", javac.display()))?;
    if !status.success() {
        std::process::exit(status.code().unwrap_or(1));
    }
    println!(
        "{}",
        crate::console::green(&format!(
            "compiled {} source files -> {}",
            sources.len(),
            out_dir.display()
        ))
    );
    Ok(())
}

/// The configured source directory, defaulting to `src/main/java`.
pub fn source_dir(config: &ProjectConfig) -> PathBuf {
    config
        .project
        .source
        .as_deref()
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(DEFAULT_SOURCE_DIR))
}

/// Whether the project has any `.java` sources under its source directory.
/// Aggregator/BOM/parent modules without sources return `false`.
pub fn has_sources(config: &ProjectConfig) -> bool {
    !collect_java_files(&source_dir(config)).is_empty()
}

/// Whether the project has sources under `src/main/kotlin` (unsupported by
/// jip, which compiles only Java sources).
pub fn has_kotlin_sources(config: &ProjectConfig) -> bool {
    let kotlin_dir = source_dir(config).with_file_name("kotlin");
    if !kotlin_dir.is_dir() {
        return false;
    }
    let mut files = Vec::new();
    walk(&kotlin_dir, &mut files);
    files
        .iter()
        .any(|p| p.extension().is_some_and(|e| e == "kt"))
}

/// Decide what `jip run` starts.  Explicit arguments and `[project] main`
/// win; otherwise the class with a `main` method is detected, falling back
/// to a single root-level `.java` file.
pub fn resolve_main(config: &ProjectConfig, arg: Option<&str>) -> anyhow::Result<MainDecision> {
    if let Some(arg) = arg {
        return Ok(MainDecision::Run(classify(arg)));
    }
    if let Some(main) = &config.project.main {
        return Ok(MainDecision::Run(classify(main)));
    }

    let candidates = main_candidates(config)?;
    if candidates.len() > 1 {
        return Ok(MainDecision::Multiple(candidates));
    }
    if let Some(fqcn) = candidates.into_iter().next() {
        return Ok(MainDecision::Run(MainTarget::Class(fqcn)));
    }

    // Fall back to a single `.java` file in the project root.
    let root_files = root_java_files();
    if root_files.len() == 1 {
        return Ok(MainDecision::Run(MainTarget::SourceFile(
            root_files[0].clone(),
        )));
    }
    Ok(MainDecision::None)
}

/// The fully qualified names of every class with a `public static void main`
/// method under the project's source directory, sorted for stable ordering.
pub fn main_candidates(config: &ProjectConfig) -> anyhow::Result<Vec<String>> {
    let mut candidates: Vec<String> = collect_java_files(&source_dir(config))
        .into_iter()
        .filter(|path| {
            fs::read_to_string(path)
                .map(|content| has_main_method(&content))
                .unwrap_or(false)
        })
        .map(|path| fqcn_of(&path))
        .collect::<anyhow::Result<_>>()?;
    candidates.sort();
    Ok(candidates)
}

/// The error message for an ambiguous `main` class, with one candidate per
/// line.  Also used for the list shown by the interactive picker.
pub fn multiple_main_error(candidates: &[String]) -> String {
    let listed: Vec<String> = candidates
        .iter()
        .map(|fqcn| format!("  - {fqcn}"))
        .collect();
    format!(
        "multiple classes with a `main` method found:\n{}\nset [project] main to pick one",
        listed.join("\n")
    )
}

/// Is `arg` a main class of this project: an existing `.java`/`.jar` file,
/// the configured `[project] main`, or a class with a detected `main` method?
///
/// Everything else passed to `jip run` is a program argument, so
/// `jip run start` runs the configured main class with `start` as its first
/// argument instead of trying to load a class named `start`.
pub fn is_main_class(config: &ProjectConfig, arg: &str) -> bool {
    let path = Path::new(arg);
    if path.extension().is_some_and(|e| e == "java" || e == "jar") {
        return true;
    }
    if matches!(config.project.main.as_deref(), Some(main) if main == arg) {
        return true;
    }
    main_candidates(config)
        .unwrap_or_default()
        .iter()
        .any(|candidate| candidate == arg)
}

/// Let the user pick one of several `main` classes by number.
///
/// Interactive only: without a terminal the ambiguity is reported as an
/// error so CI runs never hang on a prompt.
pub fn choose_main(candidates: &[String]) -> anyhow::Result<MainTarget> {
    if !std::io::stdin().is_terminal() {
        bail!("{}", multiple_main_error(candidates));
    }

    println!("multiple classes with a `main` method found — pick one:");
    for (index, fqcn) in candidates.iter().enumerate() {
        println!("  {}. {fqcn}", index + 1);
    }
    let stdin = std::io::stdin();
    loop {
        print!("  select 1-{} (q to quit): ", candidates.len());
        std::io::stdout().flush().context("cannot write prompt")?;
        let mut answer = String::new();
        if stdin
            .read_line(&mut answer)
            .context("cannot read selection")?
            == 0
        {
            bail!("no main class selected — set [project] main in jip.toml");
        }
        let answer = answer.trim();
        if answer.eq_ignore_ascii_case("q") {
            bail!("no main class selected — set [project] main in jip.toml");
        }
        if let Ok(index) = answer.parse::<usize>()
            && let Some(fqcn) = candidates.get(index.wrapping_sub(1))
        {
            return Ok(MainTarget::Class(fqcn.clone()));
        }
    }
}

/// Turn a `main` value into a run target: `.java`/`.jar` are files,
/// everything else is treated as a class name.
fn classify(main: &str) -> MainTarget {
    let path = Path::new(main);
    if path.extension().is_some_and(|e| e == "java") {
        MainTarget::SourceFile(path.to_path_buf())
    } else if path.extension().is_some_and(|e| e == "jar") {
        MainTarget::Jar(path.to_path_buf())
    } else {
        MainTarget::Class(main.to_string())
    }
}

/// All `.java` files below `dir`, sorted for deterministic builds.
pub fn collect_java_files(dir: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    if dir.is_dir() {
        walk(dir, &mut files);
    }
    files.retain(|path| path.extension().is_some_and(|e| e == "java"));
    files.sort();
    files
}

/// Every file below `dir` (the caller filters what it cares about).
fn walk(dir: &Path, files: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            walk(&path, files);
        } else {
            files.push(path);
        }
    }
}

fn root_java_files() -> Vec<PathBuf> {
    let mut files: Vec<PathBuf> = fs::read_dir(".")
        .into_iter()
        .flatten()
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.extension().is_some_and(|e| e == "java"))
        .collect();
    files.sort();
    files
}

/// Does this source define a `public static void main` method?
fn has_main_method(content: &str) -> bool {
    static RE: OnceLock<Regex> = OnceLock::new();
    let re = RE.get_or_init(|| {
        Regex::new(r"\bpublic\s+static\s+void\s+main\s*\(").expect("valid main regex")
    });
    re.is_match(content)
}

/// The fully qualified class name of a source file: the declared `package`
/// plus the file stem.
fn fqcn_of(path: &Path) -> anyhow::Result<String> {
    let content =
        fs::read_to_string(path).with_context(|| format!("cannot read {}", path.display()))?;
    let stem = path
        .file_stem()
        .and_then(|s| s.to_str())
        .with_context(|| format!("invalid source file name {}", path.display()))?;
    fqcn_from_content(&content, stem)
}

/// FQCN from file content and stem, kept separate so it can be unit tested.
fn fqcn_from_content(content: &str, stem: &str) -> anyhow::Result<String> {
    static RE: OnceLock<Regex> = OnceLock::new();
    let re = RE.get_or_init(|| {
        Regex::new(r"(?m)^\s*package\s+([\w.]+)\s*;").expect("valid package regex")
    });
    match re.captures(content).and_then(|c| c.get(1)) {
        Some(package) => Ok(format!("{}.{}", package.as_str(), stem)),
        None => Ok(stem.to_string()),
    }
}

/// Skip recompilation while the newest compiled class is newer than every
/// source, every extra input, and the `jip.toml`/`jip.lock` marker files.
fn is_up_to_date(
    sources: &[PathBuf],
    classes: &Path,
    extra_inputs: &[PathBuf],
) -> anyhow::Result<bool> {
    if !classes.is_dir() {
        return Ok(false);
    }
    let newest_source = newest_mtime(sources);
    let newest_class = newest_mtime(&collect_class_files(classes));
    let Some(source) = newest_source else {
        return Ok(false);
    };
    let Some(class) = newest_class else {
        return Ok(false);
    };
    if class < source {
        return Ok(false);
    }
    for input in extra_inputs {
        let input_mtime = if input.is_dir() {
            newest_mtime(&collect_class_files(input))
        } else {
            file_mtime(input)
        };
        if input_mtime.is_some_and(|mtime| mtime > class) {
            return Ok(false);
        }
    }
    for marker in [crate::config::CONFIG_FILE, crate::lock::LOCK_FILE] {
        let path = Path::new(marker);
        if file_mtime(path).is_some_and(|mtime| mtime > class) {
            return Ok(false);
        }
    }
    Ok(true)
}

fn collect_class_files(dir: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    if dir.is_dir() {
        walk(dir, &mut files);
    }
    files.retain(|path| path.extension().is_some_and(|e| e == "class"));
    files
}

fn newest_mtime(paths: &[PathBuf]) -> Option<std::time::SystemTime> {
    paths.iter().filter_map(|p| file_mtime(p)).max()
}

fn file_mtime(path: &Path) -> Option<std::time::SystemTime> {
    fs::metadata(path).ok()?.modified().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_main_values() {
        assert!(matches!(classify("Main.java"), MainTarget::SourceFile(_)));
        assert!(matches!(classify("app.jar"), MainTarget::Jar(_)));
        assert!(matches!(
            classify("com.example.Main"),
            MainTarget::Class(name) if name == "com.example.Main"
        ));
    }

    #[test]
    fn detects_main_method() {
        assert!(has_main_method(
            "    public static void main(String[] args) { }"
        ));
        assert!(!has_main_method("public void main(String[] args) { }"));
        assert!(!has_main_method("// public static void main"));
    }

    #[test]
    fn derives_fqcn_from_package_and_stem() {
        assert_eq!(
            fqcn_from_content("package com.example;\nclass Main {}\n", "Main").unwrap(),
            "com.example.Main"
        );
        assert_eq!(
            fqcn_from_content("class Hello {}\n", "Hello").unwrap(),
            "Hello"
        );
    }

    #[test]
    fn source_dir_defaults_to_maven_layout() {
        let config = ProjectConfig::default_config();
        assert_eq!(source_dir(&config), Path::new(DEFAULT_SOURCE_DIR));

        let mut config = ProjectConfig::default_config();
        config.project.source = Some("src/java".to_string());
        assert_eq!(source_dir(&config), Path::new("src/java"));
    }

    #[test]
    fn recognizes_main_classes() {
        let mut config = ProjectConfig::default_config();
        config.project.main = Some("com.example.Main".to_string());

        assert!(is_main_class(&config, "com.example.Main"));
        assert!(is_main_class(&config, "Main.java"));
        assert!(is_main_class(&config, "app.jar"));
        assert!(!is_main_class(&config, "start"));
    }
}
