# Building ssh-files

Detailed build instructions for all platforms.

## Prerequisites

### All Platforms

Install Rust 1.70+ via [rustup](https://rustup.rs):

```bash
# Linux/macOS
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh

# Windows - download installer from https://rustup.rs
```

### Platform-Specific Dependencies

#### Windows

No additional dependencies. MSVC build tools are included with Visual Studio or [Build Tools for Visual Studio](https://visualstudio.microsoft.com/downloads/#build-tools-for-visual-studio-2022).

#### macOS

```bash
xcode-select --install
```

#### Linux (Debian/Ubuntu)

```bash
sudo apt update
sudo apt install build-essential pkg-config libssl-dev
```

#### Linux (Fedora/RHEL)

```bash
sudo dnf install gcc pkg-config openssl-devel
```

#### Linux (Arch)

```bash
sudo pacman -S base-devel openssl
```

---

## Portable Builds

Portable builds have no external dependencies.

### Windows (Static CRT)

```batch
.\build-portable.bat
```

Or manually:
```powershell
$env:RUSTFLAGS="-C target-feature=+crt-static"
cargo build --release
```

### Linux (musl, Fully Static)

Works on any Linux distro regardless of glibc version.

```bash
# Prerequisites
rustup target add x86_64-unknown-linux-musl
sudo apt install musl-tools  # Debian/Ubuntu

# Build
./build-portable-linux.sh
```

### macOS (Universal Binary)

Single binary for Intel and Apple Silicon.

```bash
# Prerequisites
rustup target add x86_64-apple-darwin aarch64-apple-darwin

# Build
./build-portable-macos.sh
```

---

## Cross-Compilation

### Linux → Windows

```bash
rustup target add x86_64-pc-windows-gnu
sudo apt install mingw-w64
cargo build --release --target x86_64-pc-windows-gnu
```

### Using `cross` (Docker-based)

```bash
cargo install cross
cross build --release --target x86_64-unknown-linux-gnu
cross build --release --target x86_64-pc-windows-gnu
```

---

## Installation

### Linux/macOS

```bash
mkdir -p ~/.local/bin
cp target/release/ssh-files ~/.local/bin/
# Add ~/.local/bin to PATH if needed
```

### Windows

```powershell
Copy-Item target\release\ssh-files.exe $env:LOCALAPPDATA\Programs\ssh-files\
# Add to PATH in System Settings
```

---

## Troubleshooting

| Problem | Solution |
|---------|----------|
| "VCRUNTIME140.dll not found" | Use portable build or install [VC++ Redist](https://aka.ms/vs/17/release/vc_redist.x64.exe) |
| "GLIBC_X.XX not found" | Use musl portable build |
| "musl-gcc not found" | Install musl-tools for your distro |
| Colors look wrong | `export COLORTERM=truecolor` |
