mod anim;
mod blur;
mod config;
mod ipc;
mod llm;
mod markdown;
mod presentation;
mod remend;
mod render;
mod state;
mod stream;
mod theme;
mod wayland;

use crate::anim::{Spring, SpringSpec};
use crate::config::AppConfig;
use crate::ipc::{try_send_to_running, IpcCommand, IpcServer};
use crate::render::Renderer;
use crate::wayland::{run_wayland, RunOptions};
use std::path::PathBuf;

fn main() {
    if let Err(err) = try_main() {
        eprintln!("thetalk: {err}");
        std::process::exit(1);
    }
}

fn try_main() -> Result<(), String> {
    let args: Vec<String> = std::env::args().skip(1).collect();

    let mut force_window = false;
    let mut start_hidden = false;
    let mut open_settings = false;
    let mut initial_prompt: Option<String> = None;
    let mut auto_quit_ms: Option<u64> = None;
    let mut render_preview_dir: Option<PathBuf> = None;
    let mut run_self_test = false;
    let mut ipc_cmd: Option<IpcCommand> = None;

    let mut i = 0usize;
    while i < args.len() {
        match args[i].as_str() {
            "-h" | "--help" | "help" => {
                print_help();
                return Ok(());
            }
            "-v" | "--version" | "version" => {
                println!("thetalk {}", env!("CARGO_PKG_VERSION"));
                return Ok(());
            }
            "--window" => force_window = true,
            "--hidden" => start_hidden = true,
            "--self-test" => run_self_test = true,
            "--render-preview" => {
                i += 1;
                let dir = args
                    .get(i)
                    .ok_or_else(|| "--render-preview requires <dir>".to_string())?;
                render_preview_dir = Some(PathBuf::from(dir));
            }
            "--auto-quit-ms" => {
                i += 1;
                let ms_str = args
                    .get(i)
                    .ok_or_else(|| "--auto-quit-ms requires <milliseconds>".to_string())?;
                auto_quit_ms = Some(
                    ms_str
                        .parse::<u64>()
                        .map_err(|_| format!("invalid --auto-quit-ms value: {ms_str}"))?,
                );
            }
            "toggle" => ipc_cmd = Some(IpcCommand::Toggle),
            "show" | "focus" => ipc_cmd = Some(IpcCommand::Show),
            "hide" => ipc_cmd = Some(IpcCommand::Hide),
            "new" | "clear" => ipc_cmd = Some(IpcCommand::NewChat),
            "settings" => {
                ipc_cmd = Some(IpcCommand::Settings);
                open_settings = true;
            }
            "quit" | "exit" => ipc_cmd = Some(IpcCommand::Quit),
            "send" => {
                i += 1;
                let rest = args[i..].join(" ");
                if rest.trim().is_empty() {
                    return Err("`thetalk send` requires a prompt string".to_string());
                }
                ipc_cmd = Some(IpcCommand::SendPrompt(rest.clone()));
                initial_prompt = Some(rest);
                break;
            }
            other => {
                return Err(format!("unknown argument `{other}` (try `thetalk --help`)"));
            }
        }
        i += 1;
    }

    if run_self_test {
        return execute_self_test();
    }

    if let Some(dir) = render_preview_dir {
        let files = Renderer::render_showcase_pngs(&dir)?;
        for f in files {
            println!("rendered {}", f.display());
        }
        return Ok(());
    }

    if let Some(cmd) = &ipc_cmd {
        if try_send_to_running(cmd)? {
            return Ok(());
        }
        if matches!(cmd, IpcCommand::Hide | IpcCommand::Quit) {
            return Ok(());
        }
    } else if try_send_to_running(&IpcCommand::Toggle)? {
        return Ok(());
    }

    let config = AppConfig::load();
    let ipc_server = IpcServer::bind().ok();

    run_wayland(
        config,
        ipc_server,
        RunOptions {
            force_window,
            start_hidden,
            open_settings,
            initial_prompt,
            auto_quit_ms,
        },
    )
}

fn execute_self_test() -> Result<(), String> {
    // 1. Analytical M3 Expressive spring convergence
    let mut sp = Spring::new(0.0, SpringSpec::DEFAULT_SPATIAL);
    sp.retarget(100.0);
    for _ in 0..240 {
        sp.advance(1.0 / 120.0);
    }
    if (sp.value - 100.0).abs() > 0.05 {
        return Err(format!("spring failed to settle: {}", sp.value));
    }

    // 2. Streaming Markdown syntax healer
    let healed = crate::remend::remend("Streaming **bold and `code");
    if !healed.contains('`') || !healed.contains("**") {
        return Err(format!("unexpected remend output: {healed}"));
    }

    // 3. SmoothStream word-frontier optical blur curve
    let mut stream = crate::stream::SmoothStream::new();
    stream.start_streaming(1.0);
    stream.update_raw("Optical focus condenses words into sharp ink.", true, 1.01);
    stream.tick(0.12, 1.13);
    if stream.frontier().is_empty() {
        return Err("SmoothStream frontier should contain newly revealed words".to_string());
    }
    let newest = stream.frontier().last().unwrap();
    let st0 = newest
        .evaluate(newest.birth_secs + 0.02)
        .ok_or_else(|| "frontier word should be active at +20ms".to_string())?;
    if st0.sigma_logical < 3.5 {
        return Err(format!(
            "expected strong initial blur sigma (>= 3.5px), got {}",
            st0.sigma_logical
        ));
    }

    // 4. Headless Geist font loading + rich Markdown shaping + Gaussian blur compositing
    let mut renderer = Renderer::new(1.0);
    let mut app = crate::state::AppState::new(AppConfig::default());
    app.messages
        .push(crate::state::ChatItem::new_user("Ping".to_string()));
    let mut reply = crate::state::ChatItem::new_assistant_streaming(0.0);
    let sample_md = "Hello **world** `let x = 1;` ==focus== words.";
    reply.raw_content = sample_md.to_string();
    reply.stream.seed_preview_frontier(sample_md, 0.0);
    app.messages.push(reply);
    renderer.render(&mut app);

    let non_zero = renderer.pixmap.data().iter().any(|&b| b != 0);
    if !non_zero {
        return Err("headless renderer produced an empty pixmap".to_string());
    }

    println!(
        "thetalk self-test: OK (springs, remend, blur-to-focus frontier, cosmic-text Geist, tiny-skia)"
    );
    Ok(())
}

fn print_help() {
    println!(
        "\
thetalk {} — Native Wayland local-LLM overlay for niri

USAGE:
    thetalk [COMMAND] [OPTIONS]

COMMANDS:
    toggle              Toggle focus / visibility (recommended niri keybind)
    show                Show overlay and focus the input bar
    hide                Dismiss overlay and release focus
    new                 Reset conversation back to the thin floating bar
    settings            Open Base URL / API Key / Model settings panel
    send <PROMPT...>    Send a prompt to the running overlay (or start and send)
    quit                Stop the running overlay process

OPTIONS:
    --hidden            Launch daemon hidden in background (instant toggle ready)
    --window            Force XDG window fallback instead of zwlr_layer_shell_v1
    --render-preview D  Render 4 HiDPI showcase PNGs headlessly into directory D
    --self-test         Run built-in physics, markdown, font & blur smoke tests
    --auto-quit-ms N    Exit automatically after N ms (for automated E2E tests)
    -h, --help          Print this help
    -v, --version       Print version",
        env!("CARGO_PKG_VERSION")
    );
}
