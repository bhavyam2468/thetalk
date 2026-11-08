# thetalk

A native Linux Wayland local-LLM overlay built in pure Rust for [`niri`](https://github.com/YaLTeR/niri) — combining [`final-chat`](https://github.com/bhavyam2468/final-chat)'s **Quiet Paper** typography and materiality with [`DankMaterialShell`](https://github.com/AvengeMedia/DankMaterialShell)'s **Material 3 Expressive** spring physics.

No Tauri. No Electron. No WebView. Single native binary & portable `.AppImage` with embedded **Geist** and **Geist Mono** OpenType fonts.

---

## Why `thetalk` exists

Most Wayland layer-shell launchers and AI overlays hijack keyboard focus (`KeyboardInteractivity::Exclusive` or static full-surface `OnDemand`), making it impossible to keep an answer or live stream on screen while typing into your browser or editor underneath.

`thetalk` solves this directly against `niri`'s `zwlr_layer_shell_v1` focus rules:

1. **Zero-hijack passive mode (`KeyboardInteractivity::None`)**: Click into your browser or press `Esc` and `thetalk` stays floating on screen (`Layer::Top`, `exclusive_zone = -1`) while releasing keyboard interactivity completely — even if `focus-follows-mouse` is enabled in `niri`.
2. **Per-frame `wl_region` card input masking**: Transparent pixels and diffuse Gaussian drop shadows around the floating bar pass 100% of clicks and scroll events straight through to underlying windows.
3. **Focus-preserving scroll & code copy**: Scrolling Markdown responses or clicking `Copy` on a fenced code block while unfocused keeps `KeyboardInteractivity::None`, so your active window never loses focus. Hovering and clicking an editable text field cleanly re-arms `OnDemand` on the first click.

---

## Design & Features

- **Nothing until needed**: Starts as a thin floating bar anchored at the bottom center of your display — no placeholder text, no tooltips, no chrome. Typing a prompt and pressing `Enter` fluidly expands the bar upward into a conversation sheet.
- **Material 3 Expressive spring physics**: Closed-form 2×2 ODE spring solver (`SpringMath` / `RectSpring`) — frame-rate invariant across 60Hz, 120Hz, and 240Hz displays, preserving velocity on mid-flight retargets.
- **Present optical blur-to-focus streaming**: Incoming SSE deltas pass through a zero-regex streaming Markdown syntax healer (`remend`) and a smooth word-boundary pacer (`SmoothStream`). Up to 28 simultaneous frontier words condense out of a continuous-sigma separable 2D Gaussian blur (`σ = 6.6px → 0.0px` over `620ms`) into razor-sharp Geist ink.
- **Polished GFM Markdown**: Headings, paragraphs, syntax-highlighted fenced code blocks with one-click `Copy`, inline code pills, `==highlight==` marks, task lists (`- [x]`), bullet/numbered lists, blockquotes, tables, rules, and collapsible `<think>` reasoning blocks.
- **Keyless-first Settings panel**: Type `/settings` (or press `Ctrl+,` / right-click the bar) to configure `Base URL`, an **optional `API Key`** (leave empty for keyless local servers like Ollama, `llama.cpp`, LM Studio, or vLLM), one-click provider presets, automatic `GET /models` discovery, and Quiet Paper **Dark / Light** themes.

---

## Quick Start (`niri`)

### 1. Install from AppImage (or build from source)

```bash
# Build portable AppImage (~4 MB, embeds Geist & Geist Mono)
./scripts/build-appimage.sh

# Install to ~/.local/bin/thetalk + desktop entry
./scripts/install-local.sh
```

> **Build prerequisites (source builds only):** Rust `1.75+` (`rustup`) and `libxkbcommon` development headers:
> - **Arch Linux:** `sudo pacman -S rust libxkbcommon pkgconf`
> - **Fedora:** `sudo dnf install cargo rust libxkbcommon-devel pkgconf-pkg-config`
> - **Debian / Ubuntu:** `sudo apt install cargo rustc libxkbcommon-dev pkg-config`

### 2. Bind in `~/.config/niri/config.kdl`

```kdl
// Optional: start hidden in background at login for <0.5ms instant toggle
spawn-at-startup "thetalk" "--hidden"

binds {
    Mod+Space { spawn "thetalk" "toggle"; }
}
```

### 3. Controls

| Action | Input |
| :--- | :--- |
| Send prompt | `Enter` |
| Insert newline | `Shift+Enter` |
| Command menu (`/settings`, `/new`, `/theme`, `/hide`, `/quit`) | Type `/` in bar |
| Open / close Settings (`Base URL`, optional `API Key`, `Model`) | `/settings`, `Ctrl+,`, or right-click |
| Release focus to underlying `niri` window (staying visible) | `Esc` (or click outside) |
| Hide overlay | `Shift+Esc` (or `Esc` when bar is empty, or `thetalk toggle`) |
| Reset to thin floating bar | `/new` or `Ctrl+N` / `Ctrl+L` |
| Stop active generation | `Esc`, `Ctrl+C`, or click stop button |

---

## CLI & Single-Instance IPC

`thetalk` listens on `$XDG_RUNTIME_DIR/thetalk.sock` so keybinds and scripts control the running overlay in sub-millisecond time:

```bash
thetalk toggle                 # Summon focus if unfocused/hidden, or hide if focused
thetalk show                   # Reveal overlay and focus the input bar
thetalk hide                   # Dismiss overlay and release focus
thetalk new                    # Clear conversation back to thin bar
thetalk settings               # Open Settings panel
thetalk send "Explain this"    # Stream a prompt passively without stealing window focus
thetalk --render-preview out/  # Render 4 deterministic 2x HiDPI PNG previews headlessly
thetalk --self-test            # Run built-in physics, markdown, font & blur smoke tests
```

Configuration is saved with `0600` permissions at `~/.config/thetalk/settings.json`.

---

## Local Verification & Agent Workflow

To run the full test, headless preview, live `niri` focus-preservation check, screenshot capture, and `.AppImage` packaging pipeline in one command:

```bash
./scripts/verify_on_niri.sh
```

For autonomous local-model execution (pull, build, run on `niri`, debug, screenshot, log, package `.AppImage`, and push back to GitHub), see [`AGENT_CONTEXT.md`](./AGENT_CONTEXT.md) and [`LOCAL_AGENT_PROMPT.md`](./LOCAL_AGENT_PROMPT.md).
