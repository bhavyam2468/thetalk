# LOCAL_AGENT_PROMPT.md

Copy and paste the prompt block below into your local coding agent (Claude Code, OpenCode, Codex CLI, Aider, or any local LLM agent with shell access on your `niri` Linux machine).

---

```text
You are running on my local Linux machine (Wayland / niri) to complete native compilation, live compositor verification, visual QA, AppImage packaging, logging, and GitHub pushback for `bhavyam2468/thetalk`.

Follow these steps autonomously from start to finish without asking unnecessary questions:

1. PULL & READ CONTEXT
   - If not already inside the repo, clone `https://github.com/bhavyam2468/thetalk.git` and `cd thetalk`.
   - Fetch remotes and checkout branch `arena/f1ebc9ee-thetalk` (`git fetch origin && git checkout arena/f1ebc9ee-thetalk && git pull origin arena/f1ebc9ee-thetalk`).
   - Read `AGENT_CONTEXT.md` first — obey all non-negotiable UX/architecture invariants (100% pure Rust + native Wayland, zero Tauri/Electron, zero placeholders/tooltips in the floating input bar, zero `niri` keystroke hijacking when unfocused, Material 3 Expressive springs, and prominent Gaussian blur-to-focus streaming).

2. VERIFY HOST PREREQUISITES
   - Ensure `cargo`/`rustc` and `pkg-config` + `libxkbcommon` headers are present (`pkg-config --libs xkbcommon`). If missing and passwordless package install isn't available, log clearly what package is needed; otherwise proceed immediately.

3. BUILD, UNIT TEST & DEBUG
   - Run `cargo test` and `cargo build --release`.
   - If ANY compiler error, warning, borrow/type mismatch, or failing test occurs, fix it cleanly in `src/` while preserving every design requirement in `AGENT_CONTEXT.md`, and re-run until `cargo test` and `cargo build --release` succeed with zero errors.
   - Run `./target/release/thetalk --self-test` and ensure it exits 0.

4. VISUAL & HEADLESS RENDER QA
   - Run `./target/release/thetalk --render-preview assets/screenshots` to generate:
     - `assets/screenshots/01-thin-bar.png`
     - `assets/screenshots/02-streaming-blur-focus.png`
     - `assets/screenshots/03-markdown-light-paper.png`
     - `assets/screenshots/04-settings-keyless.png`
   - If you have vision/image-reading capability, inspect each generated PNG to verify:
     (a) `01-thin-bar.png` is a clean, finished thin floating bar at bottom center with NO placeholder text or clutter,
     (b) `02-streaming-blur-focus.png` clearly shows trailing frontier words condensing from soft Gaussian blur into sharp Geist typography,
     (c) `03-markdown-light-paper.png` renders warm paper Light mode, code syntax highlighting with `Copy`, inline `==zero-copy==` highlight, and task checkboxes cleanly,
     (d) `04-settings-keyless.png` renders Base URL, optional/keyless API Key, provider pills, and model chips with clean alignment and zero text clipping.
   - Polish any visual alignment or spacing issues in `src/render.rs` if you spot anything less than "finished", then re-render.

5. LIVE WAYLAND / NIRI ZERO-HIJACK & STREAMING TEST + SCREENSHOTS + LOGS
   - Run `./scripts/verify_on_niri.sh` (which starts the keyless SSE mock LLM server `scripts/mock_llm_server.py`, runs all Cargo & runtime tests, launches `thetalk` on `zwlr_layer_shell_v1` if `WAYLAND_DISPLAY` is set, verifies via `niri msg -j focused-window` that passive streaming does NOT steal keyboard focus from the underlying `niri` window, captures live screenshots via `grim` if installed, builds `releases/thetalk-x86_64.AppImage`, smoke-tests the AppImage, and writes the full diagnostic transcript to `logs/verification.log`).
   - If an Ollama / llama.cpp / LM Studio server is also running locally (e.g. `127.0.0.1:11434`), test a real keyless completion against it as well and append the result to `logs/verification.log`.
   - If any Wayland/`niri` focus, input-region, HiDPI scale, or AppImage issue surfaces during live testing, fix it in `src/` or `scripts/`, re-run `./scripts/verify_on_niri.sh`, and confirm all checks pass.

6. PUSH BUILD, CODE CHANGES, SCREENSHOTS & LOGS BACK TO GITHUB
   - Activate the prepared CI workflow if not yet placed:
     `mkdir -p .github/workflows && cp packaging/github-workflow-build.yml .github/workflows/build.yml`
   - Stage source fixes (if any), `.github/workflows/build.yml`, `Cargo.lock`, `assets/screenshots/*.png`, `logs/verification.log`, and `releases/thetalk-*.AppImage*` (verify `.AppImage` size is well under GitHub's 50 MB warning threshold — it should be ~4–8 MB stripped).
   - Commit with a clear summary message on `arena/f1ebc9ee-thetalk` and push:
     `git push origin arena/f1ebc9ee-thetalk`
   - Also optionally publish or update a GitHub release `v0.1.0` attaching `releases/thetalk-*.AppImage*` via `gh release create v0.1.0 releases/thetalk-*.AppImage* --title "thetalk v0.1.0" --notes "Native Wayland niri local-LLM overlay"` (if `gh` is authenticated).
   - Print a concise final summary with commit hash, AppImage path/size, screenshots captured, and `niri` focus verification status.
```
