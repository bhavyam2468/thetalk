# AGENT_CONTEXT.md — Technical Context & Verification Blueprint for `thetalk`

**Write the code so that the evolution of this app feels modular.** Read `AGENTS.md` and `docs/UI_ARCHITECTURE_AUDIT.md` before changing behavior; shared invariants take precedence over legacy instructions below.

This file gives a local coding agent complete architectural, protocol, and debugging context for `thetalk` so it can build, verify on a live `niri` Wayland session, inspect screenshots, fix any host-specific edge cases, build the `.AppImage`, and push artifacts/logs directly back to GitHub.

---

## 1. Why This Handoff Exists

`thetalk` was architected and written in an isolated cloud sandbox that had **no `rustc`/`cargo` toolchain, no Wayland compositor socket, and restricted outbound network access** (`crates.io` unreachable). Every upstream crate tag (`smithay-client-toolkit` `v0.19.2`, `tiny-skia` `v0.11.4`, `cosmic-text` `0.14.2`, `pulldown-cmark` `0.12.2`, `ureq` `2.10.1`, and `YaLTeR/niri`) was cloned from GitHub and cross-checked statically against our 13 Rust source files, embedded SIL OFL `Geist` / `Geist Mono` fonts, and Python mock SSE server.

Your job on the local Linux + `niri` machine is to run the real native Rust compiler, execute all unit/runtime/Wayland tests, capture real screenshots, verify zero keystroke hijacking on `niri`, package `releases/thetalk-x86_64.AppImage`, write `logs/verification.log`, and push all changes, build outputs, screenshots, and logs back to GitHub.

---

## 2. Non-Negotiable Product & UX Invariants

When debugging or polishing anything locally, **never violate these user constraints**:

1. **100% Native Rust + Native Wayland UI**: Strictly **no Tauri, no Electron, no WebView, no GTK/Qt**.
2. **Zero `niri` Keystroke Hijacking**:
   - Never set `KeyboardInteractivity::Exclusive`.
   - When `thetalk` is unfocused (`has_keyboard_focus == false`) and visible on screen, the user must be able to type continuously into underlying `niri` windows (browser, terminal, IDE) without `thetalk` stealing keystrokes — even with `focus-follows-mouse` enabled in `niri`.
   - Transparent pixels and drop-shadow padding around the card must NEVER intercept clicks or scroll events (`wl_surface.set_input_region` must tightly match `card_rect.current()`).
3. **"Finished" Minimalist UI — Nothing Until Needed**:
   - **Zero placeholder text** in the floating input bar (do NOT add `"Ask anything..."` or `"Type / for commands"`).
   - **Zero tooltips**, **zero status chrome**, **zero clutter icons** on the empty bar.
   - Starts as a thin floating bar (`540 × 46` logical px, `23px` pill radius) anchored at the bottom center of the screen; sending a prompt fluidly expands it upward (`684px` wide, `16px` radius) into a chat sheet.
4. **Present Streaming Blur-to-Focus**:
   - Keep the continuous-sigma separable 2D Gaussian blur-to-focus (`src/blur.rs` + `src/stream.rs`, `WORD_FOCUS_DURATION = 0.62s`, initial `sigma_logical = 6.6px`) clearly visible and silky smooth as words stream in.
5. **Initial Scope Only**:
   - Strong Geist typography, polished GFM Markdown + code highlighting, streaming Markdown animations (`remend` + `SmoothStream` + Gaussian frontier), and the Settings panel with `Base URL` and **optional / keyless `API Key`** + `Model` selector + Dark/Light Quiet Paper theme. Do NOT bloat with extra `final-chat` tabs/sidebar features.

---

## 3. Codebase Map (`src/`)

| File | Responsibility |
| :--- | :--- |
| `src/main.rs` | CLI parser, single-instance IPC forwarding (`toggle`, `show`, `hide`, `new`, `settings`, `send`, `quit`), `--render-preview <dir>`, `--self-test`, `--hidden`, `--window`, `--auto-quit-ms`. |
| `src/wayland.rs` | `smithay-client-toolkit` `0.19.2` `calloop` event loop, `zwlr_layer_shell_v1` (`Layer::Top`, `Anchor::BOTTOM`, `exclusive_zone = -1`, namespace `"thetalk"`), XDG window fallback, `wl_shm` `SlotPool`, HiDPI `set_buffer_scale`, `ThemedPointer`, `get_keyboard_with_repeat`, per-frame `Region` input mask, and explicit-summon remapping with OnDemand eligibility while visible. |
| `src/state.rs` | `AppState`, UTF-8 cursor/selection `TextField`, `/` command menu, Settings state (`AppConfig`), conversation turns, `wants_keyboard_on_demand()`, clipboard (`wl-copy`/`wl-paste`/`xclip`/`xsel` + internal fallback). |
| `src/render.rs` | HiDPI `tiny-skia` `0.11.4` + `cosmic-text` `0.14.2` renderer, embedded `Geist` & `Geist Mono` fonts, analytical Gaussian drop shadows, monochrome film grain (`GrainLut`), GFM block layout, continuous-sigma word frontier blur compositing, and deterministic `render_showcase_pngs()`. |
| `src/markdown.rs` | `pulldown-cmark` `0.12.2` offset-aware GFM parser (`into_offset_iter()`), `==mark==` inline highlight splitter, code syntax highlighter, and byte-range mapping onto `FrontierWord` optical blur states. |
| `src/stream.rs` | `SmoothStream` adaptive word-boundary token pacer, `FrontierWord` optical focus curve (`σ = 6.6px → 0px`), and `<think>...</think>` reasoning block splitter. |
| `src/blur.rs` | Zero-allocation reusable `BlurScratch` separable 2D continuous-sigma Gaussian convolution + analytical Abramowitz-Stegun `erf` rounded-rectangle drop shadow. |
| `src/anim.rs` | Closed-form 2×2 analytical ODE spring solver (`Spring`, `RectSpring`, `LogicalRect::limit_overshoot`) ported from DankMaterialShell's Material 3 Expressive motion specs. |
| `src/remend.rs` | Zero-regex streaming Markdown syntax healer (`**`, `*`, `` ` ``, ```` ``` ````, `~~`, `[link](...)`) ported from `final-chat`. |
| `src/llm.rs` | Threaded `ureq` `2.10.1` OpenAI-compatible SSE streaming client (`POST /chat/completions`, `GET /models`), keyless-safe (`Authorization` omitted when `api_key` is empty), consecutive-role normalizer, and non-SSE JSON fallback. |
| `src/config.rs` | Atomic `0600` XDG settings persistence (`~/.config/thetalk/settings.json`) + local/cloud provider presets (`Ollama`, `llama.cpp`, `LM Studio`, `vLLM`, `OpenRouter`, `OpenAI`). |
| `src/theme.rs` | `final-chat` Quiet Paper Dark (`#1f1e1c` / `#252422` / `#e6e1d8` / `#d08a5c`) & Light (`#f2eee6` / `#f6f3ec` / `#2a2824` / `#a8623b`) tokens, premultiplied-alpha lerp, and `128×128` SplitMix64 `GrainLut`. |

---

## 4. Exact `niri` Focus Mechanics (Reference for Debugging)

Visible layers use OnDemand across the entire rounded card, not hovered-input
eligibility. niri owns actual focus and reports it through keyboard enter/leave.
Startup maps OnDemand; explicit later summons use a one-shot remap handshake.
Clicking another app releases ownership without changing eligibility or
requesting another summon. Escape always hides. Never use Exclusive.

See `docs/UI_ARCHITECTURE_AUDIT.md` for source references and live verification
requirements. The previous hover-gating/None-on-leave policy was intentionally
removed because it broke click-anywhere focus and conflated eligibility with
ownership.

---

## 5. Artifacts Expected Back on GitHub

After running verification and any fixes:
1. `Cargo.lock` — generated by Cargo so builds are reproducible.
2. `assets/screenshots/*.png` — both the 4 deterministic `--render-preview` PNGs (`01-thin-bar.png`, `02-streaming-blur-focus.png`, `03-markdown-light-paper.png`, `04-settings-keyless.png`) AND live Wayland/`niri` screenshots (`live-*.png`) if `WAYLAND_DISPLAY` + `grim` are available.
3. `logs/verification.log` — complete build, test, `niri` focus-preservation, and AppImage verification output.
4. `releases/thetalk-x86_64.AppImage` + `releases/thetalk-x86_64.AppImage.sha256` (or uploaded to a GitHub Release via `gh release create v0.1.0 releases/thetalk-*.AppImage*` if preferred; `releases/thetalk-x86_64.AppImage` is ~3–6 MB so it also fits comfortably in Git and under GitHub's 100 MB file limit).
