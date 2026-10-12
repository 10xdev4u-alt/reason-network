#!/bin/sh
# Installs tincan for macOS or Linux.
#
#   curl -fsSL <this url> | sh                      install, then set up (start or join a team)
#   curl -fsSL <this url> | sh -s -- <team link>    install, then join that team
#   curl -fsSL <this url> | sh -s -- <team link> --as <name>     the same, without questions (for agents)
#   curl -fsSL <this url> | sh -s -- init <team> --as <name>     install and start a team, without questions
#
# Environment: TINCAN_BIN_DIR (default ~/.local/bin), TINCAN_VERSION (default latest), TINCAN_REPO,
# TINCAN_DOWNLOAD_BASE (a mirror serving the release files), TINCAN_NO_PROMPT (never ask anything: updates).
set -eu

repo="${TINCAN_REPO:-asdf9384/tincan}"
bin_dir="${TINCAN_BIN_DIR:-$HOME/.local/bin}"
version="${TINCAN_VERSION:-latest}"

say() { printf '%s\n' "$*"; }
die() { say "tincan install: $*" >&2; exit 1; }

case "$(uname -s)" in
  Darwin) os=apple-darwin ;;
  Linux) os=unknown-linux-musl ;;
  *) die "tincan runs on macOS and Linux (on Windows, use WSL)" ;;
esac
case "$(uname -m)" in
  arm64 | aarch64) arch=aarch64 ;;
  x86_64 | amd64) arch=x86_64 ;;
  *) die "unsupported CPU: $(uname -m)" ;;
esac
target="$arch-$os"

if [ -n "${TINCAN_DOWNLOAD_BASE:-}" ]; then
  base="$TINCAN_DOWNLOAD_BASE"
elif [ "$version" = latest ]; then
  base="https://github.com/$repo/releases/latest/download"
else
  base="https://github.com/$repo/releases/download/$version"
fi

tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT INT TERM

say "Downloading tincan for $target..."
curl -fsSL "$base/tincan-$target.tar.gz" -o "$tmp/tincan.tar.gz" || die "download failed ($base/tincan-$target.tar.gz)"
if curl -fsSL "$base/tincan-$target.tar.gz.sha256" -o "$tmp/sum" 2>/dev/null; then
  want="$(cut -d' ' -f1 < "$tmp/sum")"
  if command -v shasum >/dev/null 2>&1; then
    got="$(shasum -a 256 "$tmp/tincan.tar.gz" | cut -d' ' -f1)"
  else
    got="$(sha256sum "$tmp/tincan.tar.gz" | cut -d' ' -f1)"
  fi
  [ "$want" = "$got" ] || die "checksum mismatch; not installing"
fi
tar -xzf "$tmp/tincan.tar.gz" -C "$tmp"
mkdir -p "$bin_dir"
install -m 755 "$tmp/tincan" "$bin_dir/tincan"
say "Installed $bin_dir/tincan ($("$bin_dir/tincan" --version))"

case ":$PATH:" in
  *":$bin_dir:"*) ;;
  *) say "Note: $bin_dir is not on your PATH. Add this to your shell profile:"
     say "  export PATH=\"$bin_dir:\$PATH\"" ;;
esac

# Hand over to tincan: it asks a few questions (name, invite code, which agents) in your terminal. Under
# `curl | sh` our stdin is the script, so read answers from the terminal itself when there is one.
if [ -z "${TINCAN_NO_PROMPT:-}" ] && { : </dev/tty; } 2>/dev/null; then
  case "${1:-}" in
    "") exec "$bin_dir/tincan" </dev/tty ;;
    -*|init|join|setup) exec "$bin_dir/tincan" "$@" </dev/tty ;;
    *) exec "$bin_dir/tincan" join "$@" </dev/tty ;;
  esac
fi
case "${1:-}" in
  "") say ""; say "Next, in a terminal: tincan" ;;
  -*|init|join|setup) exec "$bin_dir/tincan" "$@" ;;
  *) exec "$bin_dir/tincan" join "$@" ;;
esac
