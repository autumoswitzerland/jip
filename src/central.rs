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
//  jip — Maven Central Search
//  ---------------------------------------------------------------------------
//  Small wrapper around the search.maven.org API used by `jip search`, and
//  the version lookup used by `jip add` / `jip update`.  The lookup prefers
//  the newest *stable* release read from the repository's `maven-metadata.xml`,
//  falling back to the search API's "latest" (which may be a pre-release).
//
//  Project:   jip
//  Author:    autumo GmbH
//  Date:      2026-08-15
// =============================================================================

use std::cmp::Ordering;

use anyhow::Context;
use roxmltree::Document;
use serde::Deserialize;

use crate::cache::download_repo_text;

/// Base URL of the Maven Central search API.
const SEARCH_URL: &str = "https://search.maven.org/solrsearch/select";

/// One search hit, with only the fields jip needs.
#[derive(Debug, Deserialize)]
struct SearchDoc {
    #[serde(rename = "g")]
    group: String,
    #[serde(rename = "a")]
    artifact: String,
    #[serde(rename = "latestVersion")]
    latest_version: String,
}

/// The `response` object of the Solr JSON result.
#[derive(Debug, Deserialize)]
struct SearchResponse {
    response: SearchResponseBody,
}

#[derive(Debug, Deserialize)]
struct SearchResponseBody {
    docs: Vec<SearchDoc>,
}

/// A search result formatted for display.
#[derive(Debug)]
pub struct SearchResult {
    pub group: String,
    pub artifact: String,
    pub latest_version: String,
}

/// Query the search API and return up to `rows` results.
///
/// The search service is occasionally flaky, so the request is retried a few
/// times before giving up.
pub fn search(
    client: &reqwest::blocking::Client,
    query: &str,
    rows: u32,
) -> anyhow::Result<Vec<SearchResult>> {
    let url = format!("{SEARCH_URL}?rows={rows}&wt=json");
    let mut last_error = None;
    for attempt in 1..=3 {
        match client
            .get(&url)
            .query(&[("q", query)])
            .send()
            .and_then(|response| response.error_for_status())
            .and_then(|response| response.json::<SearchResponse>())
        {
            Ok(response) => {
                return Ok(response
                    .response
                    .docs
                    .into_iter()
                    .map(|doc| SearchResult {
                        group: doc.group,
                        artifact: doc.artifact,
                        latest_version: doc.latest_version,
                    })
                    .collect());
            }
            Err(err) => {
                last_error = Some(err);
                std::thread::sleep(std::time::Duration::from_millis(500 * attempt));
            }
        }
    }
    Err(last_error.expect("the loop always runs once")).context("cannot reach search.maven.org")
}

/// Look up the latest published version of one artifact, preferring a
/// stable release over a pre-release (milestone, RC, snapshot, ...).
///
/// `repos` are checked in order (custom repositories first, Maven Central
/// last); the first repository whose `maven-metadata.xml` resolves wins.
pub fn latest_version(
    client: &reqwest::blocking::Client,
    repos: &[String],
    group: &str,
    artifact: &str,
) -> anyhow::Result<Option<String>> {
    let metadata_path = format!("{}/{artifact}/maven-metadata.xml", group.replace('.', "/"));
    for repo in repos {
        if let Ok(metadata) = download_repo_text(client, repo, &metadata_path)
            && let Some(version) = parse_latest_stable(&metadata)
        {
            return Ok(Some(version));
        }
    }
    // Fall back to the search service's "latest" when no repository answers;
    // that value may include pre-releases.
    let query = format!("g:\"{group}\" AND a:\"{artifact}\"");
    let results = search(client, &query, 1)?;
    Ok(results.first().map(|r| r.latest_version.clone()))
}

/// The newest stable version listed in a `maven-metadata.xml` file.
fn parse_latest_stable(metadata: &str) -> Option<String> {
    let doc = Document::parse(metadata).ok()?;
    doc.descendants()
        .filter(|node| node.has_tag_name("version"))
        .filter_map(|node| node.text())
        .filter(|version| !is_pre_release(version))
        .max_by(|a, b| compare_versions(a, b))
        .map(str::to_string)
}

/// Is `version` a pre-release such as `1.0-M3`, `2.0-RC1`, or `1.0-SNAPSHOT`?
fn is_pre_release(version: &str) -> bool {
    const MARKERS: [&str; 10] = [
        "m",
        "rc",
        "alpha",
        "beta",
        "snapshot",
        "cr",
        "ea",
        "pre",
        "preview",
        "milestone",
    ];
    let lower = version.to_ascii_lowercase();
    let last = lower.rsplit(['-', '.', '_']).next().unwrap_or_default();
    MARKERS.iter().any(|marker| {
        last == *marker
            || last
                .strip_prefix(marker)
                .is_some_and(|rest| rest.chars().all(|c| c.is_ascii_digit()))
    })
}

/// Order two versions the way Maven does (best effort): numeric segments
/// compare numerically, text segments compare case-insensitively, and a
/// numeric segment sorts after a text one.
fn compare_versions(a: &str, b: &str) -> Ordering {
    let a: Vec<&str> = a.split(['.', '-', '_']).collect();
    let b: Vec<&str> = b.split(['.', '-', '_']).collect();
    for (x, y) in a.iter().zip(b.iter()) {
        let ordering = match (x.parse::<u64>(), y.parse::<u64>()) {
            (Ok(x), Ok(y)) => x.cmp(&y),
            (Ok(_), Err(_)) => Ordering::Greater,
            (Err(_), Ok(_)) => Ordering::Less,
            (Err(_), Err(_)) => x.to_ascii_lowercase().cmp(&y.to_ascii_lowercase()),
        };
        if ordering != Ordering::Equal {
            return ordering;
        }
    }
    a.len().cmp(&b.len())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn picks_stable_over_milestone() {
        let metadata = r#"
            <metadata>
              <versioning>
                <latest>1.13.0-M3</latest>
                <release>1.12.0</release>
                <versions>
                  <version>1.12.0</version>
                  <version>1.13.0-M1</version>
                  <version>1.13.0-M3</version>
                </versions>
              </versioning>
            </metadata>
        "#;
        assert_eq!(parse_latest_stable(metadata).as_deref(), Some("1.12.0"));
    }

    #[test]
    fn newest_stable_wins() {
        let metadata = r#"
            <metadata>
              <versioning>
                <versions>
                  <version>1.9</version>
                  <version>1.10.0</version>
                  <version>2.0.0-RC1</version>
                  <version>33.0.0-jre</version>
                  <version>32.0.1-jre</version>
                </versions>
              </versioning>
            </metadata>
        "#;
        assert_eq!(parse_latest_stable(metadata).as_deref(), Some("33.0.0-jre"));
    }

    #[test]
    fn recognises_pre_releases() {
        assert!(is_pre_release("1.13.0-M3"));
        assert!(is_pre_release("2.0.0-RC1"));
        assert!(is_pre_release("1.0-SNAPSHOT"));
        assert!(is_pre_release("3.1.0-beta2"));
        assert!(!is_pre_release("1.12.0"));
        assert!(!is_pre_release("33.0.0-jre"));
        assert!(!is_pre_release("5.6.0.Final"));
    }

    #[test]
    fn compares_numeric_segments() {
        assert_eq!(compare_versions("1.10.0", "1.9.0"), Ordering::Greater);
        assert_eq!(compare_versions("1.9.0", "1.10.0"), Ordering::Less);
        assert_eq!(compare_versions("1.10.0", "1.10.0"), Ordering::Equal);
    }
}
