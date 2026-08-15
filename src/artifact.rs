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
//  jip — Artifact Coordinates
//  ---------------------------------------------------------------------------
//  An "artifact" is a single piece of software in a Maven repository,
//  identified by group, artifact, and version (e.g. com.google.guava:guava:33.0.0-jre).
//
//  This module knows how to parse such coordinates and how to turn them
//  into the directory layout used by Maven repositories.
//
//  Project:   jip
//  Author:    autumo GmbH
//  Date:      2026-08-15
// =============================================================================

use std::fmt;

/// Maven repository layout: `group/artifact/version/`.
///
/// Example: `com.google.guava:guava:33.0.0-jre` becomes
/// `com/google/guava/guava/33.0.0-jre/`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Artifact {
    pub group: String,
    pub artifact: String,
    pub version: String,
}

impl Artifact {
    /// Parse a coordinate string in the form `group:artifact:version`.
    pub fn parse(input: &str) -> anyhow::Result<Self> {
        let parts: Vec<&str> = input.split(':').collect();
        match parts.as_slice() {
            [group, artifact, version]
                if !group.is_empty() && !artifact.is_empty() && !version.is_empty() =>
            {
                Ok(Self {
                    group: group.to_string(),
                    artifact: artifact.to_string(),
                    version: version.to_string(),
                })
            }
            _ => {
                anyhow::bail!("invalid artifact \"{input}\" — expected \"group:artifact:version\"")
            }
        }
    }

    /// Unique key used for de-duplication, e.g. `com.google.guava:guava`.
    pub fn key(&self) -> String {
        format!("{}:{}", self.group, self.artifact)
    }

    /// Directory relative to the repository root, e.g.
    /// `com/google/guava/guava/33.0.0-jre`.
    pub fn directory(&self) -> String {
        let group_path = self.group.replace('.', "/");
        format!("{group_path}/{}/{}/", self.artifact, self.version)
    }

    /// File name of the jar, e.g. `guava-33.0.0-jre.jar`.
    pub fn jar_file_name(&self) -> String {
        format!("{}-{}.jar", self.artifact, self.version)
    }

    /// File name of the POM (Maven project file), e.g. `guava-33.0.0-jre.pom`.
    pub fn pom_file_name(&self) -> String {
        format!("{}-{}.pom", self.artifact, self.version)
    }
}

impl fmt::Display for Artifact {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}:{}", self.group, self.artifact, self.version)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_full_coordinates() {
        let a = Artifact::parse("com.google.guava:guava:33.0.0-jre").unwrap();
        assert_eq!(a.key(), "com.google.guava:guava");
        assert_eq!(a.directory(), "com/google/guava/guava/33.0.0-jre/");
        assert_eq!(a.jar_file_name(), "guava-33.0.0-jre.jar");
    }

    #[test]
    fn rejects_malformed_coordinates() {
        assert!(Artifact::parse("just-one-part").is_err());
        assert!(Artifact::parse("g:a:").is_err());
    }
}
