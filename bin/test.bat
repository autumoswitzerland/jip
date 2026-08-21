@echo off
setlocal

rem ------------------------------------------------------------------------------
rem
rem Copyright (c) 2026 autumo GmbH. All rights reserved.
rem
rem Licensed under the GNU Affero General Public License v3.0 (AGPLv3).
rem See LICENSE file in the project root for full license information.
rem
rem This file is part of jip. jip is free software: you can redistribute
rem it and/or modify it under the terms of the GNU Affero General Public License
rem as published by the Free Software Foundation, either version 3 of the
rem License, or (at your option) any later version.
rem ------------------------------------------------------------------------------
rem
rem jip Test Script
rem
rem Runs the full local test suite: formatting check, clippy, and tests.
rem
rem Usage:
rem   bin\test.bat          fmt check + clippy + tests
rem   bin\test.bat --fast   tests only
rem ------------------------------------------------------------------------------

rem Change to project root (parent of bin)
cd /d "%~dp0.."

where cargo >nul 2>&1
if errorlevel 1 (
    echo cargo not found.
    echo Install Rust: https://rustup.rs
    exit /b 1
)

echo === jip Test ===
echo.

if "%~1"=="--fast" (
    echo Running tests...
    cargo test
    if errorlevel 1 exit /b %errorlevel%
) else (
    echo Checking formatting...
    cargo fmt --check
    if errorlevel 1 exit /b %errorlevel%

    echo.
    echo Running clippy...
    cargo clippy --all-targets -- -D warnings
    if errorlevel 1 exit /b %errorlevel%

    echo.
    echo Running tests...
    cargo test
    if errorlevel 1 exit /b %errorlevel%
)

echo.
echo === All green! ===

endlocal
