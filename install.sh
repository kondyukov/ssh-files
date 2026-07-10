#!/bin/sh
# Install script for ssh-files
# Usage: curl -sSL https://raw.githubusercontent.com/kondyukov/ssh-files/main/install.sh | sh
#
# Environment variables:
#   SSH_FILES_INSTALL_DIR - Installation directory (default: ~/.local/bin)
#   SSH_FILES_REPO        - GitHub repo (default: kondyukov/ssh-files)

set -e

# Configuration
REPO="${SSH_FILES_REPO:-kondyukov/ssh-files}"
INSTALL_DIR="${SSH_FILES_INSTALL_DIR:-$HOME/.local/bin}"
BINARY_NAME="ssh-files"

# Detect OS and architecture
detect_platform() {
    OS="$(uname -s | tr '[:upper:]' '[:lower:]')"
    ARCH="$(uname -m)"

    case "$OS" in
        linux*)  OS="linux" ;;
        darwin*) OS="macos" ;;
        *)
            echo "Error: Unsupported operating system: $OS"
            echo "Please build from source or use Windows installer."
            exit 1
            ;;
    esac

    case "$ARCH" in
        x86_64|amd64)  ARCH="x64" ;;
        aarch64|arm64) ARCH="arm64" ;;
        *)
            echo "Error: Unsupported architecture: $ARCH"
            exit 1
            ;;
    esac

    # macOS Intel fallback - only arm64 and x64 builds available
    if [ "$OS" = "macos" ] && [ "$ARCH" = "x64" ]; then
        ARCH="x64"
    fi

    PLATFORM="${OS}-${ARCH}"
}

# Get latest release version from GitHub
get_latest_version() {
    if command -v curl > /dev/null 2>&1; then
        VERSION=$(curl -sSL "https://api.github.com/repos/${REPO}/releases/latest" | grep '"tag_name"' | sed -E 's/.*"([^"]+)".*/\1/')
    elif command -v wget > /dev/null 2>&1; then
        VERSION=$(wget -qO- "https://api.github.com/repos/${REPO}/releases/latest" | grep '"tag_name"' | sed -E 's/.*"([^"]+)".*/\1/')
    else
        echo "Error: curl or wget required"
        exit 1
    fi

    if [ -z "$VERSION" ]; then
        echo "Error: Could not determine latest version"
        exit 1
    fi
}

# Download and extract
download() {
    URL="https://github.com/${REPO}/releases/download/${VERSION}/ssh-files-${PLATFORM}.tar.gz"
    
    echo "Downloading ssh-files ${VERSION} for ${PLATFORM}..."
    echo "URL: $URL"
    
    TEMP_DIR=$(mktemp -d)
    TEMP_FILE="${TEMP_DIR}/ssh-files.tar.gz"
    
    if command -v curl > /dev/null 2>&1; then
        curl -sSL "$URL" -o "$TEMP_FILE"
    else
        wget -q "$URL" -O "$TEMP_FILE"
    fi

    # Extract
    tar -xzf "$TEMP_FILE" -C "$TEMP_DIR"
    
    # Install
    mkdir -p "$INSTALL_DIR"
    mv "${TEMP_DIR}/${BINARY_NAME}" "${INSTALL_DIR}/"
    chmod +x "${INSTALL_DIR}/${BINARY_NAME}"
    
    # Cleanup
    rm -rf "$TEMP_DIR"
}

# Check if directory is in PATH
check_path() {
    case ":$PATH:" in
        *":$INSTALL_DIR:"*) return 0 ;;
        *) return 1 ;;
    esac
}

# Suggest PATH addition
suggest_path() {
    SHELL_NAME=$(basename "$SHELL")
    
    case "$SHELL_NAME" in
        bash)  RC_FILE="$HOME/.bashrc" ;;
        zsh)   RC_FILE="$HOME/.zshrc" ;;
        fish)  RC_FILE="$HOME/.config/fish/config.fish" ;;
        *)     RC_FILE="$HOME/.profile" ;;
    esac

    echo ""
    echo "Add the following to $RC_FILE:"
    
    if [ "$SHELL_NAME" = "fish" ]; then
        echo "  set -gx PATH \$PATH $INSTALL_DIR"
    else
        echo "  export PATH=\"\$PATH:$INSTALL_DIR\""
    fi
    
    echo ""
    echo "Then restart your shell or run:"
    echo "  source $RC_FILE"
}

# Main
main() {
    echo "ssh-files installer"
    echo "==================="
    echo ""

    detect_platform
    echo "Detected platform: $PLATFORM"

    get_latest_version
    echo "Latest version: $VERSION"
    echo "Install directory: $INSTALL_DIR"
    echo ""

    download

    echo ""
    echo "✓ Successfully installed ssh-files to ${INSTALL_DIR}/${BINARY_NAME}"

    if ! check_path; then
        echo ""
        echo "⚠ Warning: $INSTALL_DIR is not in your PATH"
        suggest_path
    else
        echo ""
        echo "Run 'ssh-files --help' to get started."
    fi
}

main
