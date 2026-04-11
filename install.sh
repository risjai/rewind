#!/bin/sh
# ⏪ Rewind — Install Script
# Usage: curl -fsSL https://raw.githubusercontent.com/agentoptics/rewind/master/install.sh | sh

set -e

REPO="agentoptics/rewind"
BINARY="rewind"
INSTALL_DIR="${REWIND_INSTALL_DIR:-/usr/local/bin}"

# Colors
RED='\033[0;31m'
GREEN='\033[0;32m'
CYAN='\033[0;36m'
DIM='\033[2m'
BOLD='\033[1m'
RESET='\033[0m'

info() { printf "${CYAN}${BOLD}▶${RESET} %s\n" "$1"; }
success() { printf "${GREEN}${BOLD}✓${RESET} %s\n" "$1"; }
error() { printf "${RED}${BOLD}✗${RESET} %s\n" "$1" >&2; exit 1; }

# Detect OS and arch
detect_platform() {
    OS="$(uname -s)"
    ARCH="$(uname -m)"

    case "$OS" in
        Linux)  OS="linux" ;;
        Darwin) OS="darwin" ;;
        *)      error "Unsupported OS: $OS" ;;
    esac

    case "$ARCH" in
        x86_64|amd64)  ARCH="x86_64" ;;
        arm64|aarch64) ARCH="aarch64" ;;
        *)             error "Unsupported architecture: $ARCH" ;;
    esac

    PLATFORM="${OS}-${ARCH}"
}

# Get latest release tag
get_latest_version() {
    VERSION=$(curl -fsSL "https://api.github.com/repos/${REPO}/releases/latest" | grep '"tag_name"' | sed -E 's/.*"v?([^"]+)".*/\1/')
    if [ -z "$VERSION" ]; then
        error "Failed to fetch latest version"
    fi
}

# Download and install
install() {
    TARBALL="rewind-v${VERSION}-${PLATFORM}.tar.gz"
    URL="https://github.com/${REPO}/releases/download/v${VERSION}/${TARBALL}"

    info "Downloading Rewind v${VERSION} for ${PLATFORM}..."

    TMPDIR=$(mktemp -d)
    trap "rm -rf $TMPDIR" EXIT

    curl -fsSL "$URL" -o "${TMPDIR}/${TARBALL}" || error "Download failed. URL: ${URL}"

    CHECKSUM_URL="${URL}.sha256"
    info "Verifying checksum..."
    curl -fsSL "$CHECKSUM_URL" -o "${TMPDIR}/${TARBALL}.sha256" || error "Checksum download failed. URL: ${CHECKSUM_URL}"

    # Verify checksum (supports both shasum and sha256sum)
    cd "${TMPDIR}"
    if command -v shasum >/dev/null 2>&1; then
        shasum -a 256 -c "${TARBALL}.sha256" || error "Checksum verification failed — download may be corrupted or tampered with"
    elif command -v sha256sum >/dev/null 2>&1; then
        sha256sum -c "${TARBALL}.sha256" || error "Checksum verification failed — download may be corrupted or tampered with"
    else
        info "Warning: no shasum/sha256sum found, skipping checksum verification"
    fi
    cd - >/dev/null

    info "Installing to ${INSTALL_DIR}..."

    tar -xzf "${TMPDIR}/${TARBALL}" -C "${TMPDIR}"

    if [ -w "$INSTALL_DIR" ]; then
        mv "${TMPDIR}/${BINARY}" "${INSTALL_DIR}/${BINARY}"
    else
        sudo mv "${TMPDIR}/${BINARY}" "${INSTALL_DIR}/${BINARY}"
    fi

    chmod +x "${INSTALL_DIR}/${BINARY}"
}

main() {
    printf "\n"
    printf "${CYAN}${BOLD}  ⏪ Rewind Installer${RESET}\n"
    printf "${DIM}  Time-travel debugger for AI agents${RESET}\n"
    printf "\n"

    detect_platform
    get_latest_version
    install

    printf "\n"
    success "Rewind v${VERSION} installed to ${INSTALL_DIR}/${BINARY}"
    printf "\n"
    printf "  Get started:\n"
    printf "    ${GREEN}rewind demo${RESET}              ${DIM}# seed demo data${RESET}\n"
    printf "    ${GREEN}rewind inspect latest${RESET}    ${DIM}# interactive TUI${RESET}\n"
    printf "    ${GREEN}rewind record${RESET}            ${DIM}# start recording${RESET}\n"
    printf "\n"
}

main
