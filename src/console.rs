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
//  jip — Console Output
//  ---------------------------------------------------------------------------
//  Colored, structured terminal output.  Colors are only applied when the
//  terminal supports them: stdout and stderr must both be a TTY, `NO_COLOR`
//  must be unset, and `TERM` must not be `dumb`.  Everything falls back to
//  plain text when output is redirected or piped.
//
//  Project:   jip
//  Author:    autumo GmbH
//  Date:      2026-08-16
// =============================================================================

use std::io::IsTerminal;
use std::sync::OnceLock;

const RED: &str = "\x1b[31m";
const YELLOW: &str = "\x1b[33m";
const GREEN: &str = "\x1b[32m";
const BOLD: &str = "\x1b[1m";
const RESET: &str = "\x1b[0m";

/// Whether ANSI colors may be used, decided once per process.
fn enabled() -> bool {
    static ENABLED: OnceLock<bool> = OnceLock::new();
    *ENABLED.get_or_init(|| {
        if std::env::var_os("NO_COLOR").is_some_and(|v| !v.is_empty()) {
            return false;
        }
        if std::env::var_os("TERM").is_some_and(|term| term == "dumb") {
            return false;
        }
        let terminal = std::io::stdout().is_terminal() && std::io::stderr().is_terminal();
        if terminal {
            #[cfg(windows)]
            let _ = enable_ansi_support::enable_ansi_support();
        }
        terminal
    })
}

fn style(text: &str, code: &str) -> String {
    if enabled() {
        format!("{code}{text}{RESET}")
    } else {
        text.to_string()
    }
}

/// Wrap `text` in red for errors.
pub fn red(text: &str) -> String {
    style(text, RED)
}

/// Wrap `text` in yellow for warnings.
pub fn yellow(text: &str) -> String {
    style(text, YELLOW)
}

/// Wrap `text` in green for success messages.
pub fn green(text: &str) -> String {
    style(text, GREEN)
}

/// Wrap `text` in bold for emphasis.
pub fn bold(text: &str) -> String {
    style(text, BOLD)
}

/// Print a warning with the familiar `warning:` prefix.
pub fn warn(message: &str) {
    println!("{} {message}", yellow("warning:"));
}
