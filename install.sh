#!/bin/sh
# Installs ramet for the current user, then prepares its data volume.
#
#   ./install.sh                        from a clone: builds ramet with cargo
#   RAMET_BINARY=<path> ./install.sh    installs a binary built elsewhere
#
# ramet goes to ~/.local/bin (RAMET_INSTALL_DIR chooses another directory),
# then `ramet setup` prepares the data volume, asking for your password once.
# Running the script again updates ramet and changes nothing else.
set -eu

INSTALL_DIR=${RAMET_INSTALL_DIR:-$HOME/.local/bin}
SOURCE_DIR=$(cd "$(dirname "$0")" && pwd)
LABEL_WIDTH=14
# Where details continue on the next line: past the mark and the label.
PAD=$(printf "%$((LABEL_WIDTH + 5))s" '')

if [ -t 1 ]; then
  esc=$(printf '\033')
  B="$esc[1m" D="$esc[2m" R="$esc[31m" G="$esc[32m" Y="$esc[33m" C="$esc[36m" Z="$esc[0m"
else
  B='' D='' R='' G='' Y='' C='' Z=''
fi

# Same layout as `ramet setup`, which runs right after.
step()    { printf "  %s %s%-${LABEL_WIDTH}s%s %s\n" "$1" "$B" "$2" "$Z" "$3"; }
ok()      { step "${G}✓${Z}" "$1" "$2"; }
pending() { step "${C}·${Z}" "$1" "$2"; }
warn()    { step "${Y}!${Z}" "$1" "$2"; }
detail()  { printf '%s%s\n' "$PAD" "$1"; }
die()     { step "${R}✗${Z}" "$1" "$2"; echo; exit 1; }

# `path` with the home directory written `~`.
tilde() {
  case $1 in
    "$HOME"/*) printf '~/%s' "${1#"$HOME"/}" ;;
    *) printf '%s' "$1" ;;
  esac
}

echo
printf '  %sramet install%s  %sbuilds ramet and puts it in your PATH%s\n' "$B" "$Z" "$D" "$Z"
echo

[ "$(uname -s)" = Linux ] || die system "ramet runs on Linux only (btrfs, loop devices)"

# ------------------------------------------------------------------ binary --
if [ -n "${RAMET_BINARY:-}" ]; then
  [ -x "$RAMET_BINARY" ] || die binary "$RAMET_BINARY is not an executable file"
  binary=$RAMET_BINARY
  ok binary "$(tilde "$binary")"
elif grep -qs '^name = "ramet"' "$SOURCE_DIR/Cargo.toml"; then
  command -v cargo >/dev/null 2>&1 \
    || die build "cargo not found: install Rust (https://rustup.rs), then run this script again"
  version=$(sed -n 's/^version = "\(.*\)"/\1/p' "$SOURCE_DIR/Cargo.toml" | head -n 1)
  pending build "compiling ramet $version (a minute or two the first time)…"
  if ! (cd "$SOURCE_DIR" && cargo build --release --locked --quiet); then
    die build "the build failed: the errors are above"
  fi
  binary=$SOURCE_DIR/target/release/ramet
  ok build "built ramet $version"
else
  die binary "run this script from a clone of the ramet repository"
fi

# ----------------------------------------------------------------- install --
target=$INSTALL_DIR/ramet
mkdir -p "$INSTALL_DIR"
# Copied next to its destination, then renamed over it: a ramet running at
# that moment keeps its own file, and no one ever sees half a binary.
cp "$binary" "$target.new.$$"
chmod 755 "$target.new.$$"
mv -f "$target.new.$$" "$target"
ok install "$(tilde "$target")"

case ":$PATH:" in
  *":$INSTALL_DIR:"*)
    found=$(command -v ramet 2>/dev/null || true)
    if [ -n "$found" ] && [ "$found" != "$target" ]; then
      warn PATH "another ramet comes first in your PATH: $found"
      detail "remove it, or the shell will keep running that one"
    fi
    ;;
  *)
    warn PATH "$(tilde "$INSTALL_DIR") is not in your PATH; add it to your shell:"
    shell=${SHELL:-sh}
    case ${shell##*/} in
      zsh)  detail "${C}echo 'export PATH=\"$INSTALL_DIR:\$PATH\"' >> ~/.zshrc${Z}" ;;
      fish) detail "${C}fish_add_path $INSTALL_DIR${Z}" ;;
      *)    detail "${C}echo 'export PATH=\"$INSTALL_DIR:\$PATH\"' >> ~/.bashrc${Z}" ;;
    esac
    detail "then open a new terminal"
    ;;
esac

# ------------------------------------------------------------------- setup --
exec "$target" setup
