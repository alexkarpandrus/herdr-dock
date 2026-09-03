#!/bin/sh
# Build the herdr-dock binary. Prefer a local Cargo build (developers, source
# installs); otherwise fetch a prebuilt binary from GitHub Releases so end
# users do not need Rust installed.
set -eu

out_dir="target/release"
bin="$out_dir/herdr-dock"

if command -v cargo >/dev/null 2>&1; then
    echo "herdr-dock: building with Cargo"
    cargo build --release
    exit 0
fi

case "$(uname -s)" in
    Darwin) os="apple-darwin" ;;
    Linux)  os="unknown-linux-gnu" ;;
    *)
        echo "herdr-dock: unsupported platform $(uname -s)" >&2
        exit 1
        ;;
esac

case "$(uname -m)" in
    x86_64|amd64) arch="x86_64" ;;
    arm64|aarch64) arch="aarch64" ;;
    *)
        echo "herdr-dock: unsupported architecture $(uname -m)" >&2
        exit 1
        ;;
esac

target="$arch-$os"
tarball="herdr-dock-$target.tar.gz"
url="https://github.com/alexkarpandrus/herdr-dock/releases/latest/download/$tarball"

if ! command -v curl >/dev/null 2>&1; then
    echo "herdr-dock: Cargo is not installed and curl is unavailable to fetch a prebuilt binary." >&2
    exit 1
fi

echo "herdr-dock: downloading prebuilt binary for $target"
tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT

if ! curl -fsSL "$url" -o "$tmp/$tarball"; then
    echo "herdr-dock: no prebuilt binary for $target and Cargo is not installed." >&2
    echo "Install Rust (https://rustup.rs) and retry, or report this platform." >&2
    exit 1
fi

mkdir -p "$out_dir"
tar -xzf "$tmp/$tarball" -C "$out_dir"
chmod +x "$bin"
echo "herdr-dock: installed prebuilt binary to $bin"
