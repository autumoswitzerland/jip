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
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;

use anyhow::{Context, bail};
use regex::Regex;

use crate::commands::{check_java_version, classpath_for, classpath_string, load_config};
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

/// The `jip build` command: compile everything, unless already up to date.
pub fn run(client: &reqwest::blocking::Client) -> anyhow::Result<()> {
    let config = match convert::offer_conversion(client)? {
        ConversionOffer::Converted(config) => config,
        ConversionOffer::Declined => return Ok(()),
        ConversionOffer::Proceed => load_config()?,
    };
    let classpath = classpath_for(client, &config)?;
    check_java_version(config.project.java.as_deref())?;
    compile(&config, &classpath)
}

/// Compile the project's sources into `target/classes`, skipping the work
/// when the output is already newer than every source and marker file.
pub fn compile(config: &ProjectConfig, classpath: &[PathBuf]) -> anyhow::Result<()> {
    let source_dir = source_dir(config);
    let sources = collect_java_files(&source_dir);
    if sources.is_empty() {
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
            "up to date — {} source files in {}",
            sources.len(),
            out_dir.display()
        );
        return Ok(());
    }

    fs::create_dir_all(out_dir).with_context(|| format!("cannot create {}", out_dir.display()))?;
    let status = Command::new("javac")
        .arg("-d")
        .arg(out_dir)
        .arg("-classpath")
        .arg(classpath_string(classpath))
        .args(sources)
        .status()
        .context("failed to start `javac` — is a JDK installed?")?;
    if !status.success() {
        std::process::exit(status.code().unwrap_or(1));
    }
    println!(
        "compiled {} source files -> {}",
        sources.len(),
        out_dir.display()
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

/// Decide what `jip run` starts.  Explicit arguments and `[project] main`
/// win; otherwise the class with a `main` method is detected, falling back
/// to a single root-level `.java` file.
///
/// Returns `None` when nothing runnable could be found.
pub fn main_target(
    config: &ProjectConfig,
    arg: Option<&str>,
) -> anyhow::Result<Option<MainTarget>> {
    if let Some(arg) = arg {
        return Ok(Some(classify(arg)));
    }
    if let Some(main) = &config.project.main {
        return Ok(Some(classify(main)));
    }

    let candidates: Vec<String> = collect_java_files(&source_dir(config))
        .into_iter()
        .filter(|path| {
            fs::read_to_string(path)
                .map(|content| has_main_method(&content))
                .unwrap_or(false)
        })
        .map(|path| fqcn_of(&path))
        .collect::<anyhow::Result<_>>()?;
    if candidates.len() > 1 {
        bail!(
            "multiple classes with a `main` method: {} — set [project] main",
            candidates.join(", ")
        );
    }
    if let Some(fqcn) = candidates.into_iter().next() {
        return Ok(Some(MainTarget::Class(fqcn)));
    }

    // Fall back to a single `.java` file in the project root.
    let root_files = root_java_files();
    if root_files.len() == 1 {
        return Ok(Some(MainTarget::SourceFile(root_files[0].clone())));
    }
    Ok(None)
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
}
