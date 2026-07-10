@echo off
REM Build normal Windows binary (requires VC++ runtime on target system)
REM Smaller binary but needs Visual C++ Redistributable installed

echo Building ssh-files...
cargo build --release

if %ERRORLEVEL% EQU 0 (
    echo.
    echo Success! Binary at: target\release\ssh-files.exe
    echo Note: Target system needs Visual C++ Redistributable installed.
) else (
    echo Build failed!
    exit /b 1
)
