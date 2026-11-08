#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT_DIR"

PREFIX_BIN="${HOME}/.local/bin"
PREFIX_APPS="${HOME}/.local/share/applications"
PREFIX_ICONS="${HOME}/.local/share/icons/hicolor/scalable/apps"

mkdir -p "$PREFIX_BIN" "$PREFIX_APPS" "$PREFIX_ICONS"

if [ -x "$ROOT_DIR/releases/thetalk-$(uname -m).AppImage" ]; then
    cp "$ROOT_DIR/releases/thetalk-$(uname -m).AppImage" "$PREFIX_BIN/thetalk"
elif [ -x "$ROOT_DIR/target/release/thetalk" ]; then
    cp "$ROOT_DIR/target/release/thetalk" "$PREFIX_BIN/thetalk"
else
    echo "Building release binary..."
    cargo build --release
    cp "$ROOT_DIR/target/release/thetalk" "$PREFIX_BIN/thetalk"
fi

chmod 0755 "$PREFIX_BIN/thetalk"
cp "$ROOT_DIR/packaging/thetalk.desktop" "$PREFIX_APPS/thetalk.desktop"
cp "$ROOT_DIR/assets/icons/thetalk.svg" "$PREFIX_ICONS/thetalk.svg"

cat <<EOF
Installed thetalk to: $PREFIX_BIN/thetalk

Recommended ~/.config/niri/config.kdl keybind:

binds {
    Mod+Space { spawn "thetalk" "toggle"; }
}
EOF
