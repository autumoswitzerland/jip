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
rem jip Make Script
rem
rem Builds jip for the current platform.
rem
rem Usage:
rem   bin\make.bat           release build (default)
rem   bin\make.bat --debug   fast debug build
rem ------------------------------------------------------------------------------

rem Change to project root (parent of bin)
cd /d "%~dp0.."

where cargo >nul 2>&1
if errorlevel 1 (
    echo cargo not found.
    echo Install Rust: https://rustup.rs
    exit /b 1
)

set "MODE=release"

if "%~1"=="--debug" (
    set "MODE=debug"
)

echo === jip Build (%MODE%) ===
echo.

if "%MODE%"=="release" (
    cargo build --release
    if errorlevel 1 exit /b %errorlevel%
    set "BIN=target\release\jip.exe"
) else (
    cargo build
    if errorlevel 1 exit /b %errorlevel%
    set "BIN=target\debug\jip.exe"
)

echo.
echo === Done! ===
echo Binary: %CD%\%BIN%

endlocal
