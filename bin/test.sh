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
# jip Test Script
#
# Runs the full local test suite: formatting check, clippy, and tests.
#
# Usage:
#   ./bin/test.sh           # fmt check + clippy + tests
#   ./bin/test.sh --fast    # tests only
#

set -e

cd "$(dirname "$0")/.."

if ! command -v cargo >/dev/null 2>&1; then
    if [ -f "$HOME/.cargo/env" ]; then
        source "$HOME/.cargo/env"
    else
        echo "cargo not found. Install Rust: https://rustup.rs"
        exit 1
    fi
fi

echo "=== jip Test ==="
echo ""

if [ "$1" = "--fast" ]; then
    echo "Running tests..."
    cargo test
else
    echo "Checking formatting..."
    cargo fmt --check
    echo ""
    echo "Running clippy..."
    cargo clippy --all-targets -- -D warnings
    echo ""
    echo "Running tests..."
    cargo test
fi

echo ""
echo "=== All green! ==="
