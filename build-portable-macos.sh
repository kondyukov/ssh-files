#!/bin/bash
# Build portable macOS binary (universal: x86_64 + ARM64)
# Compatible with macOS 11.0+ (Big Sur and later)
#
# Prerequisites:
#   rustup target add x86_64-apple-darwin aarch64-apple-darwin

set -e

MIN_MACOS="11.0"

echo "Building portable macOS binary (universal)..."
echo "Minimum macOS version: $MIN_MACOS"
echo ""

# Install targets if needed
for TARGET in x86_64-apple-darwin aarch64-apple-darwin; do
    if ! rustup target list --installed | grep -q "$TARGET"; then
        echo "Installing target: $TARGET"
        rustup target add "$TARGET"
    fi
done

# Set minimum macOS version for compatibility
export MACOSX_DEPLOYMENT_TARGET="$MIN_MACOS"

# Build for Intel
echo "Building for x86_64 (Intel)..."
cargo build --release --target x86_64-apple-darwin

# Build for Apple Silicon
echo "Building for aarch64 (Apple Silicon)..."
cargo build --release --target aarch64-apple-darwin

# Create universal binary
echo "Creating universal binary..."
mkdir -p target/universal/release

lipo -create \
    target/x86_64-apple-darwin/release/ssh-files \
    target/aarch64-apple-darwin/release/ssh-files \
    -output target/universal/release/ssh-files

BINARY="target/universal/release/ssh-files"

if [ -f "$BINARY" ]; then
    echo ""
    echo "Build successful!"
    echo "Binary: $BINARY"
    echo ""
    echo "Binary info:"
    file "$BINARY"
    ls -lh "$BINARY"
    echo ""
    echo "Architectures:"
    lipo -info "$BINARY"
    echo ""
    echo "Minimum macOS version:"
    otool -l "$BINARY" | grep -A 3 LC_BUILD_VERSION | head -8
else
    echo "ERROR: Build failed, binary not found"
    exit 1
fi

echo ""
echo "Individual architecture binaries also available:"
echo "  Intel:         target/x86_64-apple-darwin/release/ssh-files"
echo "  Apple Silicon: target/aarch64-apple-darwin/release/ssh-files"
