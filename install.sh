#!/bin/sh
# Installs comms (company-comms) for macOS or Linux.
#
#   curl -fsSL <this url> | sh                      install, then set up (start or join a team)
#   curl -fsSL <this url> | sh -s -- <invite code>  install, then join that team
#
# Environment: COMMS_BIN_DIR (default ~/.local/bin), COMMS_VERSION (default latest), COMMS_REPO,
# COMMS_DOWNLOAD_BASE (a mirror serving the release files).
set -eu

repo="${COMMS_REPO:-asdf9384/company-comms}"
bin_dir="${COMMS_BIN_DIR:-$HOME/.local/bin}"
version="${COMMS_VERSION:-latest}"

say() { printf '%s\n' "$*"; }
die() { say "comms install: $*" >&2; exit 1; }

case "$(uname -s)" in
  Darwin) os=apple-darwin ;;
  Linux) os=unknown-linux-musl ;;
  *) die "comms runs on macOS and Linux (on Windows, use WSL)" ;;
esac
case "$(uname -m)" in
  arm64 | aarch64) arch=aarch64 ;;
  x86_64 | amd64) arch=x86_64 ;;
  *) die "unsupported CPU: $(uname -m)" ;;
esac
target="$arch-$os"

if [ -n "${COMMS_DOWNLOAD_BASE:-}" ]; then
  base="$COMMS_DOWNLOAD_BASE"
elif [ "$version" = latest ]; then
  base="https://github.com/$repo/releases/latest/download"
else
  base="https://github.com/$repo/releases/download/$version"
fi

tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT INT TERM

say "Downloading comms for $target..."
curl -fsSL "$base/comms-$target.tar.gz" -o "$tmp/comms.tar.gz" || die "download failed ($base/comms-$target.tar.gz)"
if curl -fsSL "$base/comms-$target.tar.gz.sha256" -o "$tmp/sum" 2>/dev/null; then
  want="$(cut -d' ' -f1 < "$tmp/sum")"
  if command -v shasum >/dev/null 2>&1; then
    got="$(shasum -a 256 "$tmp/comms.tar.gz" | cut -d' ' -f1)"
  else
    got="$(sha256sum "$tmp/comms.tar.gz" | cut -d' ' -f1)"
  fi
  [ "$want" = "$got" ] || die "checksum mismatch; not installing"
fi
tar -xzf "$tmp/comms.tar.gz" -C "$tmp"
mkdir -p "$bin_dir"
install -m 755 "$tmp/comms" "$bin_dir/comms"
say "Installed $bin_dir/comms ($("$bin_dir/comms" --version))"

case ":$PATH:" in
  *":$bin_dir:"*) ;;
  *) say "Note: $bin_dir is not on your PATH. Add this to your shell profile:"
     say "  export PATH=\"$bin_dir:\$PATH\"" ;;
esac

# Hand over to comms: it asks a few questions (name, invite code, which agents) in your terminal. Under
# `curl | sh` our stdin is the script, so read answers from the terminal itself when there is one.
if { : </dev/tty; } 2>/dev/null; then
  case "${1:-}" in
    "") exec "$bin_dir/comms" </dev/tty ;;
    -*) exec "$bin_dir/comms" "$@" </dev/tty ;;
    *) exec "$bin_dir/comms" join "$@" </dev/tty ;;
  esac
fi
case "${1:-}" in
  "") say ""; say "Next, in a terminal: comms" ;;
  -*) exec "$bin_dir/comms" "$@" ;;
  *) exec "$bin_dir/comms" join "$@" ;;
esac
