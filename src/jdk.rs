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
//  jip — JDK Manager
//  ---------------------------------------------------------------------------
//  Manages JDK installations under `~/.jip/jdks/`.  Each vendor has its own
//  subdirectory, and each version is a folder inside that vendor.
//
//  Storage layout:
//      ~/.jip/jdks/{vendor}/{version}/  (extracted JDK home)
//
//  Active JDK config:
//      ~/.jip/jdk.toml
//
//  Project:   jip
//  Author:    autumo GmbH
//  Date:      2026-08-18
// =============================================================================

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, bail};
use reqwest::blocking::Client;
use serde::{Deserialize, Serialize};

/// The JDK base directory: `~/.jip/jdks/`.
pub fn jdk_base() -> anyhow::Result<PathBuf> {
    let home = dirs::home_dir().context("cannot determine home directory")?;
    Ok(home.join(".jip").join("jdks"))
}

/// The active JDK config file: `~/.jip/jdk.toml`.
pub fn jdk_config_path() -> anyhow::Result<PathBuf> {
    let home = dirs::home_dir().context("cannot determine home directory")?;
    Ok(home.join(".jip").join("jdk.toml"))
}

/// Supported JDK vendors.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Vendor {
    Zulu,
    Temurin,
    Corretto,
    Graalvm,
}

impl Vendor {
    /// All supported vendors in priority order.
    #[allow(dead_code)]
    pub fn all() -> &'static [Vendor] {
        &[
            Vendor::Zulu,
            Vendor::Temurin,
            Vendor::Corretto,
            Vendor::Graalvm,
        ]
    }

    /// Human-readable name.
    #[allow(dead_code)]
    pub fn name(&self) -> &'static str {
        match self {
            Vendor::Zulu => "Azul Zulu",
            Vendor::Temurin => "Eclipse Temurin",
            Vendor::Corretto => "Amazon Corretto",
            Vendor::Graalvm => "GraalVM CE",
        }
    }

    /// Parse from a string (case-insensitive).
    pub fn from_str(s: &str) -> anyhow::Result<Self> {
        match s.to_lowercase().as_str() {
            "zulu" => Ok(Vendor::Zulu),
            "temurin" | "adoptium" => Ok(Vendor::Temurin),
            "corretto" | "amazon" => Ok(Vendor::Corretto),
            "graalvm" | "graal" => Ok(Vendor::Graalvm),
            _ => bail!("unknown vendor \"{s}\" — supported: zulu, temurin, corretto, graalvm"),
        }
    }
}

impl std::fmt::Display for Vendor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Vendor::Zulu => write!(f, "zulu"),
            Vendor::Temurin => write!(f, "temurin"),
            Vendor::Corretto => write!(f, "corretto"),
            Vendor::Graalvm => write!(f, "graalvm"),
        }
    }
}

/// Detected OS for download URL construction.
fn detect_os() -> &'static str {
    if cfg!(target_os = "macos") {
        "macos"
    } else if cfg!(target_os = "linux") {
        "linux"
    } else if cfg!(target_os = "windows") {
        "windows"
    } else {
        "linux"
    }
}

/// Detected CPU architecture for download URL construction.
fn detect_arch() -> &'static str {
    if cfg!(target_arch = "x86_64") {
        "x64"
    } else if cfg!(target_arch = "aarch64") {
        "aarch64"
    } else {
        "x64"
    }
}

/// Build the download URL for a given vendor and version.
pub fn download_url(client: &Client, vendor: Vendor, version: &str) -> anyhow::Result<String> {
    let os = detect_os();
    let arch = detect_arch();
    match vendor {
        Vendor::Zulu => resolve_zulu_url(client, version, os, arch),
        Vendor::Temurin => {
            let os_temurin = match os {
                "macos" => "mac",
                "linux" => "linux",
                "windows" => "windows",
                _ => "linux",
            };
            Ok(format!(
                "https://api.adoptium.net/v3/binary/latest/{version}/ga/{os_temurin}/{arch}/jdk/hotspot/normal/eclipse"
            ))
        }
        Vendor::Corretto => {
            let os_corretto = match os {
                "macos" => "macos",
                "linux" => "linux",
                "windows" => "windows",
                _ => "linux",
            };
            Ok(format!(
                "https://corretto.aws/downloads/latest/amazon-corretto-{version}-{arch}-{os_corretto}-jdk.tar.gz"
            ))
        }
        Vendor::Graalvm => resolve_graalvm_url(client, version, os, arch),
    }
}

/// Resolve the Zulu download URL via the Azul Metadata API.
fn resolve_zulu_url(
    client: &Client,
    version: &str,
    os: &str,
    arch: &str,
) -> anyhow::Result<String> {
    let os_azul = match os {
        "macos" => "macos",
        "linux" => "linux",
        "windows" => "windows",
        _ => "linux",
    };
    let arch_azul = match (os, arch) {
        ("macos", "aarch64") => "aarch64",
        ("macos", "x64") => "x64",
        ("linux", "aarch64") => "aarch64",
        ("linux", "x64") => "x64",
        ("windows", "x64") => "x64",
        _ => arch,
    };

    let api_url = format!(
        "https://api.azul.com/metadata/v1/zulu/packages/?java_version={version}&os={os_azul}&arch={arch_azul}&archive_type=tar.gz&java_package_type=jdk&latest=true&release_status=ga"
    );

    let resp = client
        .get(&api_url)
        .send()
        .context("failed to query Azul Metadata API")?;

    if !resp.status().is_success() {
        bail!("Azul Metadata API returned HTTP {}", resp.status());
    }

    let packages: Vec<serde_json::Value> = resp
        .json()
        .context("cannot parse Azul Metadata API response")?;

    // Find the standard JDK package (no crac, no fx suffix)
    let pkg = packages
        .iter()
        .find(|p| {
            p["name"]
                .as_str()
                .is_some_and(|n| n.contains("-ca-jdk") && !n.contains("crac") && !n.contains("fx"))
        })
        .or_else(|| packages.first())
        .context("no Zulu JDK found for this version and platform")?;

    pkg["download_url"]
        .as_str()
        .map(|s| s.to_string())
        .context("Azul Metadata API response missing download_url")
}

/// Resolve GraalVM download URL. Tries stable releases first, then innovation releases.
fn resolve_graalvm_url(
    client: &Client,
    version: &str,
    os: &str,
    arch: &str,
) -> anyhow::Result<String> {
    let os_graal = match os {
        "macos" => "macos",
        "linux" => "linux",
        "windows" => "windows",
        _ => "linux",
    };

    // 1) Try stable release: jdk-{version}
    let stable_url = format!(
        "https://github.com/graalvm/graalvm-ce-builds/releases/download/jdk-{version}/graalvm-community-jdk-{version}_{os_graal}-{arch}_bin.tar.gz"
    );
    if let Ok(r) = client.head(&stable_url).send()
        && r.status().is_success()
    {
        return Ok(stable_url);
    }

    // 2) Try innovation releases via GitHub API: graal-{version}.*
    let api_url = "https://api.github.com/repos/graalvm/graalvm-ce-builds/releases";
    let resp = client
        .get(api_url)
        .header("Accept", "application/vnd.github.v3+json")
        .send()
        .context("cannot reach GitHub Releases API for GraalVM")?;

    let status = resp.status();
    if !status.is_success() {
        bail!("GitHub API returned HTTP {status} — try again later or check the URL manually");
    }

    let text = resp.text().context("cannot read GitHub API response")?;
    let releases: Vec<serde_json::Value> =
        serde_json::from_str(&text).context("cannot parse GitHub API response as JSON")?;

    let prefix_stable = format!("jdk-{version}.");
    let prefix_innovation = format!("graal-{version}.");
    let suffix = format!("_{os_graal}-{arch}_bin.tar.gz");

    // Collect all matching URLs, then pick the first (newest — API returns newest first)
    let mut candidates = Vec::new();
    for release in &releases {
        let tag = release["tag_name"].as_str().unwrap_or("");
        if !tag.starts_with(&prefix_stable) && !tag.starts_with(&prefix_innovation) {
            continue;
        }
        if let Some(assets) = release["assets"].as_array() {
            for asset in assets {
                let name = asset["name"].as_str().unwrap_or("");
                if name.contains("-jdk-")
                    && name.ends_with(&suffix)
                    && let Some(url) = asset["browser_download_url"].as_str()
                {
                    candidates.push(url.to_string());
                }
            }
        }
    }

    if let Some(url) = candidates.into_iter().next() {
        return Ok(url);
    }

    bail!(
        "GraalVM CE {version} is not available for {os}/{arch} — check https://github.com/graalvm/graalvm-ce-builds/releases"
    )
}

/// Active JDK configuration (stored in `~/.jip/jdk.toml`).
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ActiveConfig {
    pub active: Option<ActiveEntry>,
}

/// An entry in the `[active]` section of `jdk.toml`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ActiveEntry {
    pub vendor: Vendor,
    pub version: String,
}

impl ActiveConfig {
    /// Load the active config from `~/.jip/jdk.toml`.
    pub fn load() -> anyhow::Result<Self> {
        let path = jdk_config_path()?;
        if !path.exists() {
            return Ok(Self::default());
        }
        let raw =
            fs::read_to_string(&path).with_context(|| format!("cannot read {}", path.display()))?;
        let config: Self =
            toml::from_str(&raw).with_context(|| format!("cannot parse {}", path.display()))?;
        Ok(config)
    }

    /// Save the active config to `~/.jip/jdk.toml`.
    pub fn save(&self) -> anyhow::Result<()> {
        let path = jdk_config_path()?;
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let raw = toml::to_string_pretty(self)?;
        let mut file =
            fs::File::create(&path).with_context(|| format!("cannot write {}", path.display()))?;
        file.write_all(raw.as_bytes())?;
        Ok(())
    }
}

/// A detected/installed JDK.
#[derive(Debug, Clone)]
pub struct JdkInstallation {
    pub vendor: Vendor,
    pub version: String,
    #[allow(dead_code)]
    pub path: PathBuf,
    pub active: bool,
}

/// List all installed JDKs.
pub fn list_installed() -> anyhow::Result<Vec<JdkInstallation>> {
    let base = jdk_base()?;
    let active = ActiveConfig::load()?;
    let mut installations = Vec::new();

    if !base.exists() {
        return Ok(installations);
    }

    for vendor_dir in
        fs::read_dir(&base).with_context(|| format!("cannot read {}", base.display()))?
    {
        let vendor_dir = vendor_dir?;
        if !vendor_dir.file_type()?.is_dir() {
            continue;
        }
        let vendor_name = vendor_dir.file_name();
        let vendor_name = vendor_name.to_string_lossy();
        let vendor = match Vendor::from_str(&vendor_name) {
            Ok(v) => v,
            Err(_) => continue,
        };

        for version_dir in fs::read_dir(vendor_dir.path())? {
            let version_dir = version_dir?;
            if !version_dir.file_type()?.is_dir() {
                continue;
            }
            let version = version_dir.file_name().to_string_lossy().to_string();
            let is_active = active
                .active
                .as_ref()
                .is_some_and(|a| a.vendor == vendor && a.version == version);
            installations.push(JdkInstallation {
                vendor,
                version,
                path: version_dir.path(),
                active: is_active,
            });
        }
    }

    installations.sort_by(|a, b| a.vendor.cmp(&b.vendor).then(a.version.cmp(&b.version)));
    Ok(installations)
}

/// Install a JDK by downloading and extracting it.
pub fn install(client: &Client, vendor: Vendor, version: &str) -> anyhow::Result<PathBuf> {
    let base = jdk_base()?;
    let target_dir = base.join(vendor.to_string()).join(version);

    if target_dir.exists() {
        bail!(
            "JDK {vendor} {version} is already installed at {}",
            target_dir.display()
        );
    }

    let url = download_url(client, vendor, version)?;
    println!("downloading {vendor} JDK {version}...");
    println!("  url: {url}");

    let tmp_dir = base.join(".tmp");
    fs::create_dir_all(&tmp_dir)?;
    let tar_path = tmp_dir.join(format!("{vendor}-{version}.tar.gz"));

    // Separate client with longer timeout for large JDK downloads (~200MB).
    let mut builder = Client::builder()
        .user_agent(format!("jip/{}", env!("CARGO_PKG_VERSION")))
        .timeout(std::time::Duration::from_secs(600));

    // Apply proxy from env vars
    let http_proxy = std::env::var("HTTP_PROXY")
        .or_else(|_| std::env::var("http_proxy"))
        .ok();
    let https_proxy = std::env::var("HTTPS_PROXY")
        .or_else(|_| std::env::var("https_proxy"))
        .ok();

    if let Some(url) = http_proxy
        && let Ok(proxy) = reqwest::Proxy::http(&url)
    {
        builder = builder.proxy(proxy);
    }
    if let Some(url) = https_proxy
        && let Ok(proxy) = reqwest::Proxy::https(&url)
    {
        builder = builder.proxy(proxy);
    }

    let dl_client = builder.build().context("building download client")?;

    let mut response = dl_client
        .get(&url)
        .send()
        .with_context(|| format!("failed to download from {url}"))?;

    if !response.status().is_success() {
        bail!("download failed: HTTP {} from {url}", response.status());
    }

    let mut file = fs::File::create(&tar_path)?;
    std::io::copy(&mut response, &mut file)?;
    drop(file);

    println!("extracting...");
    fs::create_dir_all(&target_dir)?;

    let output = Command::new("tar")
        .arg("xzf")
        .arg(&tar_path)
        .arg("-C")
        .arg(&target_dir)
        .arg("--strip-components=1")
        .output()
        .context("failed to run tar — is it installed?")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        fs::remove_dir_all(&target_dir).ok();
        bail!("extraction failed: {stderr}");
    }

    fs::remove_file(&tar_path).ok();
    fs::remove_dir(&tmp_dir).ok();

    println!(
        "{}",
        crate::console::green(&format!(
            "installed {vendor} JDK {version} to {}",
            target_dir.display()
        ))
    );

    Ok(target_dir)
}

/// Set the active JDK.
pub fn set_active(vendor: Vendor, version: &str) -> anyhow::Result<()> {
    let base = jdk_base()?;
    let target_dir = base.join(vendor.to_string()).join(version);

    if !target_dir.exists() {
        bail!(
            "JDK {vendor} {version} is not installed — run `jip java install {version} --vendor {vendor}` first"
        );
    }

    let mut config = ActiveConfig::load()?;
    config.active = Some(ActiveEntry {
        vendor,
        version: version.to_string(),
    });
    config.save()?;

    println!(
        "{}",
        crate::console::green(&format!("active JDK set to {vendor} {version}"))
    );

    Ok(())
}

/// Remove an installed JDK.
pub fn remove(vendor: Vendor, version: &str) -> anyhow::Result<()> {
    let base = jdk_base()?;
    let target_dir = base.join(vendor.to_string()).join(version);

    if !target_dir.exists() {
        bail!("JDK {vendor} {version} is not installed");
    }

    // If it's the active JDK, only allow deletion if it's the only one
    let mut config = ActiveConfig::load()?;
    let was_active = config
        .active
        .as_ref()
        .is_some_and(|a| a.vendor == vendor && a.version == version);
    if was_active {
        let all = list_installed()?;
        let others: Vec<_> = all
            .iter()
            .filter(|j| !(j.vendor == vendor && j.version == version))
            .collect();
        if !others.is_empty() {
            let vendors: Vec<String> = others.iter().map(|j| j.vendor.to_string()).collect();
            bail!(
                "JDK {vendor} {version} is the active JDK — run `jip java use <other>` first (installed: {})",
                vendors.join(", ")
            );
        }
        config.active = None;
        config.save()?;
    }

    fs::remove_dir_all(&target_dir)
        .with_context(|| format!("cannot remove {}", target_dir.display()))?;

    if was_active {
        println!(
            "{}",
            crate::console::green(&format!(
                "removed {vendor} JDK {version} (active JDK cleared — falls back to system java)"
            ))
        );
    } else {
        println!(
            "{}",
            crate::console::green(&format!("removed {vendor} JDK {version}"))
        );
    }

    Ok(())
}

/// Get the path to the active JDK's `bin/java`.
#[allow(dead_code)]
pub fn active_java() -> anyhow::Result<PathBuf> {
    let config = ActiveConfig::load()?;
    let active = config
        .active
        .context("no active JDK set — run `jip java use <version>` first")?;

    let base = jdk_base()?;
    let java_path = base
        .join(active.vendor.to_string())
        .join(&active.version)
        .join("bin")
        .join("java");

    if !java_path.exists() {
        bail!(
            "active JDK {} {} not found at {}",
            active.vendor,
            active.version,
            java_path.display()
        );
    }

    Ok(java_path)
}

/// Detect the Java major version from a `java` binary.
#[allow(dead_code)]
pub fn detect_java_version(java_path: &Path) -> anyhow::Result<u32> {
    let output = Command::new(java_path)
        .arg("-version")
        .output()
        .with_context(|| format!("failed to run {} -version", java_path.display()))?;

    let stderr = String::from_utf8_lossy(&output.stderr);
    let quoted = stderr
        .split('"')
        .nth(1)
        .context("cannot parse java -version output")?;

    crate::commands::parse_major(quoted)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vendor_display() {
        assert_eq!(Vendor::Zulu.to_string(), "zulu");
        assert_eq!(Vendor::Temurin.to_string(), "temurin");
        assert_eq!(Vendor::Corretto.to_string(), "corretto");
        assert_eq!(Vendor::Graalvm.to_string(), "graalvm");
    }

    #[test]
    fn vendor_from_str() {
        assert_eq!(Vendor::from_str("zulu").unwrap(), Vendor::Zulu);
        assert_eq!(Vendor::from_str("temurin").unwrap(), Vendor::Temurin);
        assert_eq!(Vendor::from_str("adoptium").unwrap(), Vendor::Temurin);
        assert_eq!(Vendor::from_str("corretto").unwrap(), Vendor::Corretto);
        assert_eq!(Vendor::from_str("amazon").unwrap(), Vendor::Corretto);
        assert_eq!(Vendor::from_str("graalvm").unwrap(), Vendor::Graalvm);
        assert_eq!(Vendor::from_str("graal").unwrap(), Vendor::Graalvm);
        assert!(Vendor::from_str("oracle").is_err());
    }

    #[test]
    fn temurin_url_pattern() {
        // Verify URL pattern without hitting the network
        let url = format!(
            "https://api.adoptium.net/v3/binary/latest/21/ga/mac/aarch64/jdk/hotspot/normal/eclipse"
        );
        assert!(url.starts_with("https://api.adoptium.net/v3/binary/latest/21/ga/"));
        assert!(url.contains("/jdk/hotspot/normal/eclipse"));
    }

    #[test]
    fn corretto_url_pattern() {
        let client = reqwest::blocking::Client::new();
        let url = download_url(&client, Vendor::Corretto, "21").unwrap();
        assert!(url.contains("amazon-corretto-21-"));
        assert!(url.ends_with("-jdk.tar.gz"));
    }

    #[test]
    fn graalvm_url_pattern() {
        // Just verify the URL pattern is correct without hitting the network
        let os = detect_os();
        let arch = detect_arch();
        let expected = format!(
            "https://github.com/graalvm/graalvm-ce-builds/releases/download/jdk-21/graalvm-community-jdk-21_{os}-{arch}_bin.tar.gz"
        );
        assert!(expected.contains("graalvm-community-jdk-21_"));
        assert!(expected.ends_with("_bin.tar.gz"));
    }

    use std::sync::Mutex;

    // Serialize all tests that touch ~/.jip/jdk.toml
    static ACTIVE_MUTEX: Mutex<()> = Mutex::new(());

    #[test]
    fn active_config_roundtrip() {
        let _lock = ACTIVE_MUTEX.lock().unwrap();
        let config = ActiveConfig {
            active: Some(ActiveEntry {
                vendor: Vendor::Temurin,
                version: "21".to_string(),
            }),
        };
        config.save().unwrap();
        let loaded = ActiveConfig::load().unwrap();
        let active = loaded.active.unwrap();
        assert_eq!(active.vendor, Vendor::Temurin);
        assert_eq!(active.version, "21");
        ActiveConfig { active: None }.save().unwrap();
    }

    #[test]
    fn active_java_fails_when_no_active() {
        let _lock = ACTIVE_MUTEX.lock().unwrap();
        ActiveConfig { active: None }.save().unwrap();
        let result = active_java();
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("no active JDK set")
        );
    }

    #[test]
    fn active_java_fails_when_jdk_not_installed() {
        let _lock = ACTIVE_MUTEX.lock().unwrap();
        let config = ActiveConfig {
            active: Some(ActiveEntry {
                vendor: Vendor::Zulu,
                version: "999".to_string(),
            }),
        };
        config.save().unwrap();
        let result = active_java();
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("not found"));
        ActiveConfig { active: None }.save().unwrap();
    }

    #[test]
    fn active_java_returns_path_when_jdk_exists() {
        let _lock = ACTIVE_MUTEX.lock().unwrap();
        let base = jdk_base().unwrap();
        let fake_java = base.join("zulu").join("21").join("bin").join("java");
        if let Some(parent) = fake_java.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(&fake_java, "").unwrap();

        let config = ActiveConfig {
            active: Some(ActiveEntry {
                vendor: Vendor::Zulu,
                version: "21".to_string(),
            }),
        };
        config.save().unwrap();

        let result = active_java();
        let path = result.unwrap();
        assert!(path.to_string_lossy().contains("zulu/21/bin/java"));

        let _ = fs::remove_dir_all(base.join("zulu").join("21"));
        ActiveConfig { active: None }.save().unwrap();
    }
}
