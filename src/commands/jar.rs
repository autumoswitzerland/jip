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
//  jip — `jip jar`
//  ---------------------------------------------------------------------------
//  Packages the project's compiled classes into a jar file.
//
//    * Thin jar:  `target/app.jar` from `target/classes` only.
//    * Fat jar:   `target/app-fat.jar` with all dependency jars merged in.
//
//  After a thin jar is built, the user is asked whether to add it to the
//  runtime classpath via `[classpath] extra`.
//
//  Project:   jip
//  Author:    autumo GmbH
//  Date:      2026-08-17
// =============================================================================

use std::collections::BTreeSet;
use std::fs;
use std::io::{IsTerminal, Write};
use std::path::Path;

use anyhow::{Context, bail};
use zip::ZipArchive;
use zip::write::SimpleFileOptions;

use crate::commands::build;
use crate::commands::{classpath_for, compile_classpath_for, load_config};
use crate::config::{CONFIG_FILE, ProjectConfig};
use crate::console;
use crate::convert::{self, ConversionOffer};

/// The thin jar output path.
const JAR_PATH: &str = "target/app.jar";
/// The fat jar output path.
const FAT_JAR_PATH: &str = "target/app-fat.jar";

/// The `jip jar` command.
pub fn run(client: &reqwest::blocking::Client, offline: bool, fat: bool) -> anyhow::Result<()> {
    let mut config = match convert::offer_conversion(client)? {
        ConversionOffer::Converted(config) => config,
        ConversionOffer::Declined => return Ok(()),
        ConversionOffer::Proceed => Box::new(load_config()?),
    };

    let compile_classpath = compile_classpath_for(client, &config, offline)?;
    build::compile(&config, &compile_classpath)?;

    let main_class = resolve_main_class(&config)?;

    if fat {
        build_fat_jar(client, &config, &main_class, offline)?;
    } else {
        build_thin_jar(&config, &main_class)?;
        offer_classpath_extra(&mut config)?;
    }

    Ok(())
}

/// Resolve the main class for the manifest's `Main-Class` entry.
fn resolve_main_class(config: &ProjectConfig) -> anyhow::Result<Option<String>> {
    if let Some(main) = &config.project.main {
        return Ok(Some(main.clone()));
    }
    let candidates = build::main_candidates(config)?;
    if candidates.len() == 1 {
        return Ok(Some(candidates.into_iter().next().unwrap()));
    }
    Ok(None)
}

/// Build a thin jar from `target/classes`.
fn build_thin_jar(_config: &ProjectConfig, main_class: &Option<String>) -> anyhow::Result<()> {
    let classes_dir = Path::new(build::CLASSES_DIR);
    if !classes_dir.is_dir() {
        bail!(
            "{} does not exist — run `jip build` first",
            classes_dir.display()
        );
    }

    let jar_path = Path::new(JAR_PATH);
    fs::create_dir_all(jar_path.parent().unwrap())
        .with_context(|| format!("cannot create {}", jar_path.parent().unwrap().display()))?;

    let file = fs::File::create(jar_path)
        .with_context(|| format!("cannot create {}", jar_path.display()))?;
    let mut zip = zip::ZipWriter::new(file);
    let options = SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated)
        .compression_level(Some(6));

    write_manifest(&mut zip, main_class, options)?;

    let mut seen = BTreeSet::new();
    seen.insert("META-INF/MANIFEST.MF".to_string());
    let mut duplicates = Vec::<String>::new();
    add_directory_contents_tracking(
        &mut zip,
        classes_dir,
        classes_dir,
        options,
        &mut seen,
        &mut duplicates,
    )?;
    zip.finish()?;

    let size = fs::metadata(jar_path).map(|m| m.len()).unwrap_or(0);
    println!(
        "{}",
        console::green(&format!(
            "created {} ({:.1} KB)",
            jar_path.display(),
            size as f64 / 1024.0
        ))
    );
    Ok(())
}

/// Build a fat jar: project classes + all dependency jars merged.
fn build_fat_jar(
    client: &reqwest::blocking::Client,
    config: &ProjectConfig,
    main_class: &Option<String>,
    offline: bool,
) -> anyhow::Result<()> {
    let classes_dir = Path::new(build::CLASSES_DIR);
    if !classes_dir.is_dir() {
        bail!(
            "{} does not exist — run `jip build` first",
            classes_dir.display()
        );
    }

    let classpath = classpath_for(client, config, offline)?;
    let fat_path = Path::new(FAT_JAR_PATH);
    fs::create_dir_all(fat_path.parent().unwrap())
        .with_context(|| format!("cannot create {}", fat_path.parent().unwrap().display()))?;

    let file = fs::File::create(fat_path)
        .with_context(|| format!("cannot create {}", fat_path.display()))?;
    let mut zip = zip::ZipWriter::new(file);
    let options = SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated)
        .compression_level(Some(6));

    write_manifest(&mut zip, main_class, options)?;

    // Track seen entries for duplicate detection.
    let mut seen = BTreeSet::new();
    seen.insert("META-INF/MANIFEST.MF".to_string());
    let mut duplicates: Vec<String> = Vec::new();

    // Add project classes first (they win over dependency entries).
    add_directory_contents_tracking(
        &mut zip,
        classes_dir,
        classes_dir,
        options,
        &mut seen,
        &mut duplicates,
    )?;

    // Merge dependency jars.
    for jar in &classpath {
        if jar.is_file() {
            merge_jar(&mut zip, jar, &mut seen, &mut duplicates, options)?;
        }
    }

    zip.finish()?;

    if !duplicates.is_empty() {
        println!(
            "{}",
            console::yellow(&format!(
                "warning: {} duplicate resource(s) overwritten (last-wins):",
                duplicates.len()
            ))
        );
        for name in duplicates.iter().take(10) {
            println!("  {name}");
        }
        if duplicates.len() > 10 {
            println!("  ... and {} more", duplicates.len() - 10);
        }
    }

    let size = fs::metadata(fat_path).map(|m| m.len()).unwrap_or(0);
    println!(
        "{}",
        console::green(&format!(
            "created {} ({:.1} KB, {} dependencies)",
            fat_path.display(),
            size as f64 / 1024.0,
            classpath.len()
        ))
    );
    Ok(())
}

/// Write `META-INF/MANIFEST.MF` into the zip.
fn write_manifest(
    zip: &mut zip::ZipWriter<fs::File>,
    main_class: &Option<String>,
    options: SimpleFileOptions,
) -> anyhow::Result<()> {
    let mut manifest = format!(
        "Manifest-Version: 1.0\nCreated-By: jip {}\n",
        env!("CARGO_PKG_VERSION")
    );
    if let Some(main) = main_class {
        manifest.push_str(&format!("Main-Class: {main}\n"));
    }
    zip.start_file("META-INF/MANIFEST.MF", options)?;
    zip.write_all(manifest.as_bytes())?;
    Ok(())
}

/// Add all files under `dir` to the zip, with paths relative to `base`.
fn add_directory_contents_tracking(
    zip: &mut zip::ZipWriter<fs::File>,
    dir: &Path,
    base: &Path,
    options: SimpleFileOptions,
    seen: &mut BTreeSet<String>,
    duplicates: &mut Vec<String>,
) -> anyhow::Result<()> {
    let mut stack = vec![dir.to_path_buf()];
    while let Some(current) = stack.pop() {
        let Ok(entries) = fs::read_dir(&current) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
                continue;
            }
            let relative = path
                .strip_prefix(base)
                .unwrap_or(&path)
                .to_string_lossy()
                .into_owned();
            if !seen.insert(relative.clone()) {
                duplicates.push(relative);
                continue;
            }
            let mut file =
                fs::File::open(&path).with_context(|| format!("cannot open {}", path.display()))?;
            zip.start_file(&relative, options)?;
            std::io::copy(&mut file, zip)?;
        }
    }
    Ok(())
}

/// Merge a dependency jar into the fat jar, tracking duplicates.
fn merge_jar(
    zip: &mut zip::ZipWriter<fs::File>,
    jar_path: &Path,
    seen: &mut BTreeSet<String>,
    duplicates: &mut Vec<String>,
    options: SimpleFileOptions,
) -> anyhow::Result<()> {
    let file =
        fs::File::open(jar_path).with_context(|| format!("cannot open {}", jar_path.display()))?;
    let mut archive =
        ZipArchive::new(file).with_context(|| format!("invalid jar {}", jar_path.display()))?;

    for i in 0..archive.len() {
        let mut entry = archive.by_index(i)?;
        if entry.is_dir() {
            continue;
        }
        let name = entry.name().to_string();
        // Skip files that must not be merged from dependencies.
        if should_skip_entry(&name) {
            continue;
        }
        if !seen.insert(name.clone()) {
            duplicates.push(name);
            continue;
        }
        zip.start_file(&name, options)?;
        std::io::copy(&mut entry, zip)?;
    }
    Ok(())
}

/// Whether a ZIP entry should be skipped when merging into a fat jar.
///
/// Signature files (*.SF, *.DSA, *.RSA) must be excluded — they belong to
/// the original JAR's code signing and become invalid when the entry bytes
/// change during merging.  The manifest is always regenerated, and common
/// metadata like NOTICE/LICENSE would just produce duplicates.
fn should_skip_entry(name: &str) -> bool {
    let upper = name.to_ascii_uppercase();
    if upper == "META-INF/MANIFEST.MF" {
        return true;
    }
    if upper.starts_with("META-INF/") {
        let filename = upper.split('/').next_back().unwrap_or("");
        // Signature files.
        if filename.ends_with(".SF") || filename.ends_with(".DSA") || filename.ends_with(".RSA") {
            return true;
        }
        // Duplicate metadata.
        if filename == "NOTICE"
            || filename == "LICENSE"
            || filename == "LICENSE.TXT"
            || filename == "DEPENDENCIES"
        {
            return true;
        }
    }
    false
}

/// After building a thin jar, ask whether to add it to `[classpath] extra`.
fn offer_classpath_extra(config: &mut ProjectConfig) -> anyhow::Result<()> {
    if !std::io::stdin().is_terminal() {
        return Ok(());
    }
    if config.classpath.extra.iter().any(|e| e == JAR_PATH) {
        return Ok(());
    }

    print!(
        "add {} to [classpath] extra? [Y/n] ",
        console::bold(JAR_PATH)
    );
    std::io::stdout().flush().context("cannot write prompt")?;

    let mut answer = String::new();
    std::io::stdin()
        .read_line(&mut answer)
        .context("cannot read answer")?;

    let answer = answer.trim();
    if answer.is_empty() || answer.eq_ignore_ascii_case("y") || answer.eq_ignore_ascii_case("yes") {
        config.classpath.extra.push(JAR_PATH.to_string());
        config.save(Path::new(CONFIG_FILE))?;
        println!(
            "{}",
            console::green(&format!("added {JAR_PATH} to [classpath] extra"))
        );
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use zip::ZipArchive;

    fn make_classes_dir(tmp: &Path, files: &[&str]) {
        let classes = tmp.join("target/classes");
        fs::create_dir_all(&classes).unwrap();
        for file in files {
            let path = classes.join(file);
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent).unwrap();
            }
            fs::write(&path, "fake class").unwrap();
        }
    }

    #[test]
    fn resolve_main_class_from_config() {
        let mut config = ProjectConfig::default_config();
        config.project.main = Some("com.example.Main".to_string());
        assert_eq!(
            resolve_main_class(&config).unwrap(),
            Some("com.example.Main".to_string())
        );
    }

    #[test]
    fn resolve_main_class_none_when_empty() {
        let config = ProjectConfig::default_config();
        assert_eq!(resolve_main_class(&config).unwrap(), None);
    }

    #[test]
    fn thin_jar_contains_class_files() {
        let tmp = std::env::temp_dir().join("jip-test-thin-jar");
        let _ = fs::remove_dir_all(&tmp);
        make_classes_dir(&tmp, &["com/example/App.class", "com/example/Util.class"]);

        let jar_path = tmp.join("target/app.jar");
        let file = fs::File::create(&jar_path).unwrap();
        let mut zip = zip::ZipWriter::new(file);
        let options =
            SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);

        let manifest = "Manifest-Version: 1.0\nMain-Class: com.example.App\n";
        zip.start_file("META-INF/MANIFEST.MF", options).unwrap();
        zip.write_all(manifest.as_bytes()).unwrap();

        let classes_dir = tmp.join("target/classes");
        let mut seen = BTreeSet::new();
        seen.insert("META-INF/MANIFEST.MF".to_string());
        let mut duplicates = Vec::new();
        add_directory_contents_tracking(
            &mut zip,
            &classes_dir,
            &classes_dir,
            options,
            &mut seen,
            &mut duplicates,
        )
        .unwrap();
        zip.finish().unwrap();

        let file = fs::File::open(&jar_path).unwrap();
        let archive = ZipArchive::new(file).unwrap();
        let names: Vec<String> = archive.file_names().map(|n| n.to_string()).collect();
        assert!(names.contains(&"META-INF/MANIFEST.MF".to_string()));
        assert!(names.contains(&"com/example/App.class".to_string()));
        assert!(names.contains(&"com/example/Util.class".to_string()));
        assert!(duplicates.is_empty());

        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn thin_jar_skips_duplicate_manifest() {
        let tmp = std::env::temp_dir().join("jip-test-thin-dup");
        let _ = fs::remove_dir_all(&tmp);
        make_classes_dir(&tmp, &["com/example/App.class", "META-INF/MANIFEST.MF"]);

        let file = fs::File::create(tmp.join("target/app.jar")).unwrap();
        let mut zip = zip::ZipWriter::new(file);
        let options =
            SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);

        let manifest = "Manifest-Version: 1.0\n";
        zip.start_file("META-INF/MANIFEST.MF", options).unwrap();
        zip.write_all(manifest.as_bytes()).unwrap();

        let classes_dir = tmp.join("target/classes");
        let mut seen = BTreeSet::new();
        seen.insert("META-INF/MANIFEST.MF".to_string());
        let mut duplicates = Vec::new();
        add_directory_contents_tracking(
            &mut zip,
            &classes_dir,
            &classes_dir,
            options,
            &mut seen,
            &mut duplicates,
        )
        .unwrap();
        zip.finish().unwrap();

        assert_eq!(duplicates, vec!["META-INF/MANIFEST.MF".to_string()]);
        let file = fs::File::open(tmp.join("target/app.jar")).unwrap();
        let archive = ZipArchive::new(file).unwrap();
        let count = archive.file_names().count();
        assert_eq!(count, 2);

        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn fat_jar_merges_dependency() {
        let tmp = std::env::temp_dir().join("jip-test-fat-jar");
        let _ = fs::remove_dir_all(&tmp);
        make_classes_dir(&tmp, &["com/example/App.class"]);

        let dep_dir = tmp.join("deps");
        fs::create_dir_all(&dep_dir).unwrap();
        let dep_jar = dep_dir.join("lib.jar");
        {
            let file = fs::File::create(&dep_jar).unwrap();
            let mut zip = zip::ZipWriter::new(file);
            let options =
                SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
            zip.start_file("com/lib/Helper.class", options).unwrap();
            zip.write_all(b"fake dep class").unwrap();
            zip.start_file("META-INF/MANIFEST.MF", options).unwrap();
            zip.write_all(b"Manifest-Version: 1.0\n").unwrap();
            zip.finish().unwrap();
        }

        let fat_path = tmp.join("target/app-fat.jar");
        let file = fs::File::create(&fat_path).unwrap();
        let mut zip = zip::ZipWriter::new(file);
        let options =
            SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);

        // Write our own manifest (like build_fat_jar does)
        zip.start_file("META-INF/MANIFEST.MF", options).unwrap();
        zip.write_all(b"Manifest-Version: 1.0\nMain-Class: com.example.App\n")
            .unwrap();

        let mut seen = BTreeSet::new();
        seen.insert("META-INF/MANIFEST.MF".to_string());
        let mut duplicates = Vec::new();

        let classes_dir = tmp.join("target/classes");
        add_directory_contents_tracking(
            &mut zip,
            &classes_dir,
            &classes_dir,
            options,
            &mut seen,
            &mut duplicates,
        )
        .unwrap();
        merge_jar(&mut zip, &dep_jar, &mut seen, &mut duplicates, options).unwrap();
        zip.finish().unwrap();

        let file = fs::File::open(&fat_path).unwrap();
        let archive = ZipArchive::new(file).unwrap();
        let names: Vec<String> = archive.file_names().map(|n| n.to_string()).collect();
        assert!(names.contains(&"com/example/App.class".to_string()));
        assert!(names.contains(&"com/lib/Helper.class".to_string()));
        assert_eq!(
            names
                .iter()
                .filter(|n| **n == "META-INF/MANIFEST.MF")
                .count(),
            1
        );

        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn fat_jar_warns_on_duplicate_resources() {
        let tmp = std::env::temp_dir().join("jip-test-fat-dup");
        let _ = fs::remove_dir_all(&tmp);
        make_classes_dir(&tmp, &["com/example/App.class"]);

        let dep_dir = tmp.join("deps");
        fs::create_dir_all(&dep_dir).unwrap();
        let dep_jar = dep_dir.join("lib.jar");
        {
            let file = fs::File::create(&dep_jar).unwrap();
            let mut zip = zip::ZipWriter::new(file);
            let options =
                SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
            zip.start_file("com/example/App.class", options).unwrap();
            zip.write_all(b"dep version").unwrap();
            zip.finish().unwrap();
        }

        let fat_path = tmp.join("target/app-fat.jar");
        let file = fs::File::create(&fat_path).unwrap();
        let mut zip = zip::ZipWriter::new(file);
        let options =
            SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);

        let mut seen = BTreeSet::new();
        let mut duplicates = Vec::new();

        let classes_dir = tmp.join("target/classes");
        add_directory_contents_tracking(
            &mut zip,
            &classes_dir,
            &classes_dir,
            options,
            &mut seen,
            &mut duplicates,
        )
        .unwrap();
        merge_jar(&mut zip, &dep_jar, &mut seen, &mut duplicates, options).unwrap();
        zip.finish().unwrap();

        assert_eq!(duplicates.len(), 1);
        assert_eq!(duplicates[0], "com/example/App.class");

        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn fat_jar_skips_signature_and_metadata_files() {
        let tmp = std::env::temp_dir().join("jip-test-fat-skip");
        let _ = fs::remove_dir_all(&tmp);
        make_classes_dir(&tmp, &["com/example/App.class"]);

        let dep_dir = tmp.join("deps");
        fs::create_dir_all(&dep_dir).unwrap();
        let dep_jar = dep_dir.join("signed.jar");
        {
            let file = fs::File::create(&dep_jar).unwrap();
            let mut zip = zip::ZipWriter::new(file);
            let options =
                SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
            zip.start_file("com/lib/Helper.class", options).unwrap();
            zip.write_all(b"class bytes").unwrap();
            // These should be excluded.
            zip.start_file("META-INF/MANIFEST.MF", options).unwrap();
            zip.write_all(b"Manifest").unwrap();
            zip.start_file("META-INF/CERT.SF", options).unwrap();
            zip.write_all(b"sig").unwrap();
            zip.start_file("META-INF/CERT.RSA", options).unwrap();
            zip.write_all(b"sig").unwrap();
            zip.start_file("META-INF/CERT.DSA", options).unwrap();
            zip.write_all(b"sig").unwrap();
            zip.start_file("META-INF/NOTICE", options).unwrap();
            zip.write_all(b"notice").unwrap();
            zip.start_file("META-INF/LICENSE", options).unwrap();
            zip.write_all(b"license").unwrap();
            zip.finish().unwrap();
        }

        let fat_path = tmp.join("target/app-fat.jar");
        let file = fs::File::create(&fat_path).unwrap();
        let mut zip = zip::ZipWriter::new(file);
        let options =
            SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);

        let mut seen = BTreeSet::new();
        let mut duplicates = Vec::new();

        let classes_dir = tmp.join("target/classes");
        add_directory_contents_tracking(
            &mut zip,
            &classes_dir,
            &classes_dir,
            options,
            &mut seen,
            &mut duplicates,
        )
        .unwrap();
        merge_jar(&mut zip, &dep_jar, &mut seen, &mut duplicates, options).unwrap();
        zip.finish().unwrap();

        let file = fs::File::open(&fat_path).unwrap();
        let archive = ZipArchive::new(file).unwrap();
        let names: Vec<String> = archive.file_names().map(|n| n.to_string()).collect();
        assert!(names.contains(&"com/example/App.class".to_string()));
        assert!(names.contains(&"com/lib/Helper.class".to_string()));
        // Signature and metadata files must not be in the fat jar.
        assert!(
            !names
                .iter()
                .any(|n| n.ends_with(".SF") || n.ends_with(".RSA") || n.ends_with(".DSA"))
        );
        assert!(!names.contains(&"META-INF/NOTICE".to_string()));
        assert!(!names.contains(&"META-INF/LICENSE".to_string()));

        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn should_skip_signature_files() {
        assert!(should_skip_entry("META-INF/MANIFEST.MF"));
        assert!(should_skip_entry("META-INF/CERT.SF"));
        assert!(should_skip_entry("META-INF/CERT.RSA"));
        assert!(should_skip_entry("META-INF/CERT.DSA"));
        assert!(should_skip_entry("META-INF/NOTICE"));
        assert!(should_skip_entry("META-INF/LICENSE"));
        assert!(should_skip_entry("META-INF/LICENSE.TXT"));
        assert!(should_skip_entry("META-INF/DEPENDENCIES"));
        assert!(!should_skip_entry(
            "META-INF/services/javax.script.ScriptEngine"
        ));
        assert!(!should_skip_entry("com/example/App.class"));
        assert!(!should_skip_entry("META-INF/spring.factories"));
    }
}
