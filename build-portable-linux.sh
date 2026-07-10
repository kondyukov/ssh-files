#!/bin/bash
# Build portable Linux binary using musl for maximum compatibility
# Works on any Linux distro (no glibc version requirements)
#
# Prerequisites:
#   rustup target add x86_64-unknown-linux-musl
#   # Debian/Ubuntu:
#   sudo apt install musl-tools
#   # Fedora:
#   sudo dnf install musl-gcc
#   # Arch:
#   sudo pacman -S musl

set -e

TARGET="x86_64-unknown-linux-musl"

echo "Building portable Linux binary (musl)..."
echo "Target: $TARGET"
echo ""

# Check if musl target is installed
if ! rustup target list --installed | grep -q "$TARGET"; then
    echo "Installing musl target..."
    rustup target add "$TARGET"
fi

# Check for musl-gcc
if ! command -v musl-gcc &> /dev/null; then
    echo "ERROR: musl-gcc not found. Install musl-tools:"
    echo "  Debian/Ubuntu: sudo apt install musl-tools"
    echo "  Fedora:        sudo dnf install musl-gcc"
    echo "  Arch:          sudo pacman -S musl"
    exit 1
fi

# Build with static linking
# - CC_x86_64_unknown_linux_musl: Use musl-gcc
# - RUSTFLAGS: Enable static CRT and full relro for security
export CC_x86_64_unknown_linux_musl=musl-gcc
export RUSTFLAGS="-C target-feature=+crt-static -C link-self-contained=yes"

cargo build --release --target "$TARGET"

BINARY="target/$TARGET/release/ssh-files"

if [ -f "$BINARY" ]; then
    echo ""
    echo "Build successful!"
    echo "Binary: $BINARY"
    echo ""
    echo "Binary info:"
    file "$BINARY"
    ls -lh "$BINARY"
    echo ""
    echo "Dependencies (should show 'statically linked'):"
    ldd "$BINARY" 2>&1 || echo "  (statically linked - no dynamic dependencies)"
else
    echo "ERROR: Build failed, binary not found"
    exit 1
fi
