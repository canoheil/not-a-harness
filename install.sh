#!/bin/sh
# nah installer for Linux and macOS.
#
#   curl -fsSL https://raw.githubusercontent.com/canoheil/not-a-harness/main/install.sh | sh
#
# Downloads the latest release, installs the `nah` binary into
# ~/.local/bin and makes sure that directory is on your PATH, so `nah`
# can be run from any terminal. Re-running the script updates nah.
set -eu

REPO="canoheil/not-a-harness"
BASE="${NAH_DOWNLOAD_BASE:-https://github.com/$REPO/releases/latest/download}"

info() { printf '%s\n' "$*"; }
fail() { printf 'error: %s\n' "$*" >&2; exit 1; }

# --- detect platform ---------------------------------------------------
os="$(uname -s)"
arch="$(uname -m)"
case "$os" in
    Linux)  target_os="unknown-linux-gnu" ;;
    Darwin) target_os="apple-darwin" ;;
    *) fail "unsupported operating system '$os' - on Windows use install.ps1" ;;
esac
case "$arch" in
    x86_64 | amd64) target_cpu="x86_64" ;;
    aarch64 | arm64) target_cpu="aarch64" ;;
    *) fail "unsupported CPU architecture '$arch'" ;;
esac
if [ "$target_os" = "unknown-linux-gnu" ] && [ "$target_cpu" != "x86_64" ]; then
    fail "no prebuilt binary for linux-$target_cpu yet - build from source: cargo build --release"
fi
target="${target_cpu}-${target_os}"
asset="nah-${target}.tar.gz"
url="$BASE/$asset"

# --- download + extract ------------------------------------------------
info "downloading $url ..."
tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT
if command -v curl >/dev/null 2>&1; then
    curl -fsSL "$url" -o "$tmp/$asset"
elif command -v wget >/dev/null 2>&1; then
    wget -qO "$tmp/$asset" "$url"
else
    fail "need curl or wget to download nah"
fi
tar xzf "$tmp/$asset" -C "$tmp"
bin="$(find "$tmp" -type f -name nah | head -n 1)"
[ -n "$bin" ] || fail "the archive did not contain the nah binary"
chmod +x "$bin"

# --- install -----------------------------------------------------------
bindir="${NAH_INSTALL_DIR:-$HOME/.local/bin}"
mkdir -p "$bindir"
mv "$bin" "$bindir/nah"
info "installed: $bindir/nah"

# --- make `nah` reachable from any terminal ----------------------------
case ":$PATH:" in
    *":$bindir:"*)
        info "PATH already contains $bindir"
        ;;
    *)
        line="export PATH=\"$bindir:\$PATH\""
        for rc in "$HOME/.profile" "$HOME/.bashrc" "$HOME/.zshrc"; do
            if [ -f "$rc" ] || [ "$rc" = "$HOME/.profile" ]; then
                if ! grep -qsF "$line" "$rc"; then
                    printf '\n# nah\n%s\n' "$line" >>"$rc"
                fi
            fi
        done
        info "added $bindir to PATH (shell profiles updated)"
        info "restart this terminal - or run: $line"
        ;;
esac

info ""
info "done! type: nah"
