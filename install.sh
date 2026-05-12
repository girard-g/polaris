#!/usr/bin/env bash
# Polaris installer — downloads the latest release binary from GitHub
# and installs it to ~/.local/bin (default) or /usr/local/bin (--system).
#
# Usage: curl -fsSL https://raw.githubusercontent.com/girard-g/polaris/main/install.sh | sh
#        ./install.sh [--system] [--dry-run] [--version vX.Y.Z]

set -euo pipefail

REPO="girard-g/polaris"
INSTALL_DIR="${HOME}/.local/bin"
DRY_RUN=0
VERSION="latest"

usage() {
    cat <<EOF
Polaris installer

Usage:
  install.sh [--system] [--dry-run] [--version vX.Y.Z] [-h|--help]

Options:
  --system           Install to /usr/local/bin (requires sudo) instead of ~/.local/bin
  --dry-run          Print what would happen, do not download or write files
  --version vX.Y.Z   Install a specific release tag instead of the latest
  -h, --help         Show this help and exit
EOF
}

while [ $# -gt 0 ]; do
    case "$1" in
        --system) INSTALL_DIR="/usr/local/bin"; shift ;;
        --dry-run) DRY_RUN=1; shift ;;
        --version) VERSION="$2"; shift 2 ;;
        -h|--help) usage; exit 0 ;;
        *) echo "error: unknown argument: $1" >&2; usage >&2; exit 2 ;;
    esac
done

detect_asset() {
    local os arch
    os="$(uname -s)"
    arch="$(uname -m)"
    case "${os}-${arch}" in
        Linux-x86_64)  echo "polaris-linux-x86_64" ;;
        Darwin-arm64)  echo "polaris-macos-aarch64" ;;
        Darwin-aarch64) echo "polaris-macos-aarch64" ;;
        *) return 1 ;;
    esac
}

ASSET="$(detect_asset)" || {
    cat >&2 <<EOF
error: unsupported platform: $(uname -s)-$(uname -m).
Polaris release binaries are published for Linux x86_64 and macOS Apple Silicon.
Build from source instead: https://github.com/${REPO}#install-from-source
EOF
    exit 1
}

if [ "${VERSION}" = "latest" ]; then
    URL="https://github.com/${REPO}/releases/latest/download/${ASSET}"
else
    URL="https://github.com/${REPO}/releases/download/${VERSION}/${ASSET}"
fi

TARGET="${INSTALL_DIR}/polaris"

if [ "${DRY_RUN}" -eq 1 ]; then
    echo "dry-run: would download ${URL}"
    echo "dry-run: would install to ${TARGET}"
    exit 0
fi

echo "Polaris installer"
echo "  asset:   ${ASSET}"
echo "  version: ${VERSION}"
echo "  target:  ${TARGET}"
echo

mkdir -p "${INSTALL_DIR}"

TMP="$(mktemp -d)"
trap 'rm -rf "${TMP}"' EXIT

echo "Downloading ${URL}..."
if ! curl -fSL --retry 3 -o "${TMP}/polaris" "${URL}"; then
    echo "error: download failed. Check that release ${VERSION} exists at https://github.com/${REPO}/releases" >&2
    exit 1
fi

chmod +x "${TMP}/polaris"

if [ "${INSTALL_DIR}" = "/usr/local/bin" ] && [ ! -w "${INSTALL_DIR}" ]; then
    echo "Installing to ${TARGET} (requires sudo)..."
    sudo mv "${TMP}/polaris" "${TARGET}"
else
    mv "${TMP}/polaris" "${TARGET}"
fi

echo
echo "Installed: $("${TARGET}" --version 2>/dev/null || echo "polaris (version check unavailable)")"
echo "Location:  ${TARGET}"

case ":${PATH}:" in
    *":${INSTALL_DIR}:"*) ;;
    *) echo
       echo "Note: ${INSTALL_DIR} is not on your PATH. Add it to your shell profile:"
       echo "  export PATH=\"${INSTALL_DIR}:\$PATH\""
       ;;
esac
