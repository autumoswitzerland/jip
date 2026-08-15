#!/bin/bash
# ------------------------------------------------------------------------------
# Copyright (c) 2026 autumo GmbH. All rights reserved.
#
# Licensed under the GNU Affero General Public License v3.0 (AGPLv3).
# See LICENSE file in the project root for full license information.
#
# This file is part of jip. jip is free software: you can redistribute
# it and/or modify it under the terms of the GNU Affero General Public License
# as published by the Free Software Foundation, either version 3 of the
# License, or (at your option) any later version.
# ------------------------------------------------------------------------------
#
# jip Release Script
#
# Creates a git tag and pushes it to GitHub.  This triggers the GitHub
# Actions workflow (release.yml) which cross-compiles the binaries for
# all supported platforms and creates a GitHub Release with the binaries
# attached as assets.
#
# Usage:
#   ./bin/release.sh 1.0.0
#

set -e

if [ -z "$1" ]; then
    echo "Usage: ./bin/release.sh <version>"
    echo "Example: ./bin/release.sh 1.0.0"
    exit 1
fi

VERSION="$1"

# Validate version format (e.g. 1.0.0, 1.0.1-beta.1)
if ! [[ "$VERSION" =~ ^[0-9]+\.[0-9]+\.[0-9]+(-[a-zA-Z0-9.]+)?$ ]]; then
    echo "Invalid version format: $VERSION"
    echo "Expected format: X.Y.Z (e.g. 1.0.0, 1.0.1-beta.1)"
    exit 1
fi

echo "=== jip Release ==="
echo ""

# Check for uncommitted changes
if ! git diff --quiet 2>/dev/null; then
    echo "You have uncommitted changes. Please commit first."
    exit 1
fi

echo "Tagging v${VERSION}..."
git tag "v${VERSION}"

echo "Pushing tags..."
git push --tags

echo ""
echo "=== Done! ==="
echo "GitHub Actions will now build and publish:"
echo "  - jip Linux x86_64 (v${VERSION})"
echo "  - jip Linux aarch64 (v${VERSION})"
echo "  - jip Windows x86_64 (v${VERSION})"
echo "  - jip macOS x86_64 (v${VERSION})"
echo "  - jip macOS aarch64 (v${VERSION})"
echo "  - GitHub Release (v${VERSION})"
echo ""
