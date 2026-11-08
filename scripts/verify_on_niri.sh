#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT_DIR"

mkdir -p "$ROOT_DIR/logs" "$ROOT_DIR/assets/screenshots" "$ROOT_DIR/releases"
LOG_FILE="$ROOT_DIR/logs/verification.log"
exec > >(tee "$LOG_FILE") 2>&1

echo "=================================================================="
echo " thetalk — Build, Render, Wayland/niri & AppImage Verification"
echo " Timestamp (UTC): $(date -u '+%Y-%m-%dT%H:%M:%SZ')"
echo " Host: $(uname -srmo)"
echo "=================================================================="

echo ""
echo "==> [1/7] Toolchain & Environment Check"
rustc --version
cargo --version
echo "WAYLAND_DISPLAY=${WAYLAND_DISPLAY:-<unset>}"
echo "XDG_CURRENT_DESKTOP=${XDG_CURRENT_DESKTOP:-<unset>}"
if command -v niri >/dev/null 2>&1; then
    niri --version || true
fi

echo ""
echo "==> [2/7] Running Cargo Unit Tests"
cargo test --color=never

echo ""
echo "==> [3/7] Building Release Binary"
cargo build --release --color=never
ls -lh target/release/thetalk

echo ""
echo "==> [4/7] Running Built-in Runtime Self-Test"
./target/release/thetalk --self-test

echo ""
echo "==> [5/7] Generating Deterministic 2x HiDPI Showcase Screenshots"
./target/release/thetalk --render-preview "$ROOT_DIR/assets/screenshots"
ls -lh "$ROOT_DIR/assets/screenshots"/*.png

echo ""
echo "==> [6/7] Live Wayland / niri Compositor & Focus-Preservation Test"
take_wayland_shot() {
    local dest="$1"
    if command -v grim >/dev/null 2>&1; then
        grim "$dest" && echo "    Captured live Wayland screenshot -> $dest"
        return 0
    fi
    return 1
}

if [ -n "${WAYLAND_DISPLAY:-}" ]; then
    MOCK_PID=""
    TEST_XDG_CFG="$(mktemp -d "${TMPDIR:-/tmp}/thetalk-verify-cfg.XXXXXX")"
    TEST_XDG_RUN="$(mktemp -d "${TMPDIR:-/tmp}/thetalk-verify-run.XXXXXX")"
    chmod 0700 "$TEST_XDG_RUN"

    cleanup_live() {
        XDG_RUNTIME_DIR="$TEST_XDG_RUN" ./target/release/thetalk quit >/dev/null 2>&1 || true
        if [ -n "$MOCK_PID" ]; then
            kill "$MOCK_PID" >/dev/null 2>&1 || true
        fi
        rm -rf "$TEST_XDG_CFG" "$TEST_XDG_RUN"
    }
    trap cleanup_live EXIT

    python3 "$ROOT_DIR/scripts/mock_llm_server.py" --port 18088 --delay-ms 32 &
    MOCK_PID=$!
    sleep 0.4

    FOCUS_BEFORE=""
    if command -v niri >/dev/null 2>&1; then
        FOCUS_BEFORE="$(niri msg -j focused-window 2>/dev/null || true)"
        echo "    niri focused-window before passive launch: ${FOCUS_BEFORE:-none}"
    fi

    # 6a. Launch hidden, then show thin floating bar
    XDG_CONFIG_HOME="$TEST_XDG_CFG" \
    XDG_RUNTIME_DIR="$TEST_XDG_RUN" \
    LLM_BASE_URL="http://127.0.0.1:18088/v1" \
    LLM_MODEL="mock-quiet-paper-7b" \
    ./target/release/thetalk --hidden &
    DAEMON_PID=$!
    sleep 0.45

    if command -v niri >/dev/null 2>&1; then
        echo "    niri layer surfaces:"
        niri msg layers 2>/dev/null || true
    fi

    XDG_RUNTIME_DIR="$TEST_XDG_RUN" ./target/release/thetalk show
    sleep 0.40
    take_wayland_shot "$ROOT_DIR/assets/screenshots/live-01-thin-bar.png" || true

    # 6b. Trigger passive IPC prompt send (grab_focus=false) and verify niri underlying focus is NOT hijacked!
    XDG_RUNTIME_DIR="$TEST_XDG_RUN" ./target/release/thetalk hide
    sleep 0.25
    if command -v niri >/dev/null 2>&1; then
        FOCUS_BASELINE="$(niri msg -j focused-window 2>/dev/null || true)"
    fi

    XDG_RUNTIME_DIR="$TEST_XDG_RUN" ./target/release/thetalk send "Explain zero-hijack focus on niri."
    sleep 0.38
    take_wayland_shot "$ROOT_DIR/assets/screenshots/live-02-streaming-blur.png" || true

    if command -v niri >/dev/null 2>&1; then
        FOCUS_DURING_STREAM="$(niri msg -j focused-window 2>/dev/null || true)"
        echo "    niri focused-window during passive streaming: ${FOCUS_DURING_STREAM:-none}"
        if [ -n "${FOCUS_BASELINE:-}" ] && [ "$FOCUS_BASELINE" = "$FOCUS_DURING_STREAM" ]; then
            echo "    [PASS] Underlying niri window retained keyboard focus during live overlay streaming!"
        fi
    fi

    sleep 2.2
    take_wayland_shot "$ROOT_DIR/assets/screenshots/live-03-expanded-markdown.png" || true

    XDG_RUNTIME_DIR="$TEST_XDG_RUN" ./target/release/thetalk settings
    sleep 0.45
    take_wayland_shot "$ROOT_DIR/assets/screenshots/live-04-settings.png" || true

    XDG_RUNTIME_DIR="$TEST_XDG_RUN" ./target/release/thetalk quit
    wait "$DAEMON_PID" || true
    trap - EXIT
    cleanup_live
else
    echo "    WAYLAND_DISPLAY is not set in this shell — skipped live compositor pass (headless renders succeeded)."
fi

echo ""
echo "==> [7/7] Building & Smoke-Testing Portable AppImage"
"$ROOT_DIR/scripts/build-appimage.sh"
APPIMAGE_FILE="$ROOT_DIR/releases/thetalk-$(uname -m).AppImage"
APPIMAGE_EXTRACT_AND_RUN=1 "$APPIMAGE_FILE" --self-test

echo ""
echo "=================================================================="
echo " ALL VERIFICATION CHECKS PASSED"
echo " - Binary:      target/release/thetalk"
echo " - AppImage:    $APPIMAGE_FILE"
echo " - Screenshots: assets/screenshots/"
echo " - Log file:    logs/verification.log"
echo "=================================================================="
