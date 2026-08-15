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
# jip Make Script
#
# Builds jip for the current platform.
#
# Usage:
#   ./bin/make.sh            # release build (default)
#   ./bin/make.sh --debug    # fast debug build
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

MODE="release"
if [ "$1" = "--debug" ]; then
    MODE="debug"
fi

echo "=== jip Build (${MODE}) ==="
echo ""

if [ "$MODE" = "release" ]; then
    cargo build --release
    BIN="target/release/jip"
else
    cargo build
    BIN="target/debug/jip"
fi

echo ""
echo "=== Done! ==="
echo "Binary: $(pwd)/${BIN}"
