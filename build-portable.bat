@echo off
REM Build portable Windows binary with static CRT linking
REM Produces a self-contained .exe with no external dependencies

echo Building portable ssh-files...
set RUSTFLAGS=-C target-feature=+crt-static
cargo build --release --features portable

if %ERRORLEVEL% EQU 0 (
    echo.
    echo Success! Portable binary at: target\release\ssh-files.exe
    echo This exe has no DLL dependencies and works on any Windows 7+ system.
) else (
    echo Build failed!
    exit /b 1
)
