#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT_DIR"

ARCH="${ARCH:-$(uname -m)}"
case "$ARCH" in
    x86_64|aarch64) ;;
    arm64) ARCH="aarch64" ;;
    *)
        echo "Unsupported architecture for AppImage packaging: $ARCH" >&2
        exit 1
        ;;
esac

echo "==> [1/4] Building release binary (thetalk)..."
if [ -f Cargo.lock ]; then
    cargo build --release --locked || cargo build --release
else
    cargo build --release
fi

BIN_PATH="$ROOT_DIR/target/release/thetalk"
if [ ! -x "$BIN_PATH" ]; then
    echo "Release binary not found at $BIN_PATH" >&2
    exit 1
fi

echo "==> [2/4] Staging AppDir..."
STAGE_ROOT="$(mktemp -d "${TMPDIR:-/tmp}/thetalk-appdir.XXXXXX")"
trap 'rm -rf "$STAGE_ROOT"' EXIT
APPDIR="$STAGE_ROOT/thetalk.AppDir"

mkdir -p \
    "$APPDIR/usr/bin" \
    "$APPDIR/usr/lib" \
    "$APPDIR/usr/share/applications" \
    "$APPDIR/usr/share/icons/hicolor/scalable/apps"

cp "$BIN_PATH" "$APPDIR/usr/bin/thetalk"
chmod 0755 "$APPDIR/usr/bin/thetalk"

cp "$ROOT_DIR/packaging/AppRun" "$APPDIR/AppRun"
chmod 0755 "$APPDIR/AppRun"

cp "$ROOT_DIR/packaging/thetalk.desktop" "$APPDIR/thetalk.desktop"
cp "$ROOT_DIR/packaging/thetalk.desktop" "$APPDIR/usr/share/applications/thetalk.desktop"

cp "$ROOT_DIR/assets/icons/thetalk.svg" "$APPDIR/thetalk.svg"
cp "$ROOT_DIR/assets/icons/thetalk.svg" "$APPDIR/usr/share/icons/hicolor/scalable/apps/thetalk.svg"

if command -v rsvg-convert >/dev/null 2>&1; then
    rsvg-convert -w 256 -h 256 "$ROOT_DIR/assets/icons/thetalk.svg" -o "$APPDIR/thetalk.png"
    cp "$APPDIR/thetalk.png" "$APPDIR/.DirIcon"
elif command -v convert >/dev/null 2>&1; then
    convert -background none -resize 256x256 "$ROOT_DIR/assets/icons/thetalk.svg" "$APPDIR/thetalk.png"
    cp "$APPDIR/thetalk.png" "$APPDIR/.DirIcon"
else
    cp "$ROOT_DIR/assets/icons/thetalk.svg" "$APPDIR/.DirIcon"
fi

# Bundle non-glibc leaf library (`libxkbcommon.so.0`) as a portable fallback while never bundling glibc/ld-linux
if command -v ldd >/dev/null 2>&1; then
    while IFS= read -r line; do
        lib_path="$(awk '/=> \// { print $3 }' <<<"$line")"
        [ -n "$lib_path" ] || continue
        lib_name="$(basename "$lib_path")"
        case "$lib_name" in
            libxkbcommon.so*)
                cp -L "$lib_path" "$APPDIR/usr/lib/$lib_name"
                ;;
        esac
    done < <(ldd "$BIN_PATH" || true)
fi

echo "==> [3/4] Resolving appimagetool ($ARCH)..."
APPIMAGETOOL_BIN=""
if command -v appimagetool >/dev/null 2>&1; then
    APPIMAGETOOL_BIN="$(command -v appimagetool)"
else
    TOOLS_DIR="$ROOT_DIR/.cache/tools"
    mkdir -p "$TOOLS_DIR"
    CACHED_TOOL="$TOOLS_DIR/appimagetool-${ARCH}.AppImage"
    if [ ! -x "$CACHED_TOOL" ]; then
        URL="https://github.com/AppImage/appimagetool/releases/download/continuous/appimagetool-${ARCH}.AppImage"
        echo "    Downloading $URL ..."
        curl -fL --retry 3 -o "$CACHED_TOOL" "$URL"
        chmod +x "$CACHED_TOOL"
    fi
    APPIMAGETOOL_BIN="$CACHED_TOOL"
fi

echo "==> [4/4] Generating AppImage..."
OUT_DIR="${OUT_DIR:-$ROOT_DIR/releases}"
mkdir -p "$OUT_DIR"
OUT_FILE="$OUT_DIR/thetalk-${ARCH}.AppImage"
rm -f "$OUT_FILE"

ARCH="$ARCH" APPIMAGE_EXTRACT_AND_RUN=1 "$APPIMAGETOOL_BIN" --no-appstream "$APPDIR" "$OUT_FILE"
chmod +x "$OUT_FILE"

if command -v sha256sum >/dev/null 2>&1; then
    (cd "$OUT_DIR" && sha256sum "$(basename "$OUT_FILE")" > "$(basename "$OUT_FILE").sha256")
fi

SIZE_H="$(du -h "$OUT_FILE" | cut -f1)"
echo "==> Built AppImage: $OUT_FILE ($SIZE_H)"
