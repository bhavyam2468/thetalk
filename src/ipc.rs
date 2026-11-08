use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::time::Duration;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IpcCommand {
    Toggle,
    Show,
    Hide,
    NewChat,
    Settings,
    SendPrompt(String),
    Quit,
}

impl IpcCommand {
    pub fn to_wire(&self) -> String {
        match self {
            Self::Toggle => "toggle\n".to_string(),
            Self::Show => "show\n".to_string(),
            Self::Hide => "hide\n".to_string(),
            Self::NewChat => "new\n".to_string(),
            Self::Settings => "settings\n".to_string(),
            Self::SendPrompt(p) => {
                let escaped = p.replace('\n', "\\n");
                format!("send:{}\n", escaped)
            }
            Self::Quit => "quit\n".to_string(),
        }
    }

    pub fn from_wire(line: &str) -> Option<Self> {
        let trimmed = line.trim();
        match trimmed {
            "toggle" => Some(Self::Toggle),
            "show" | "focus" => Some(Self::Show),
            "hide" => Some(Self::Hide),
            "new" | "clear" => Some(Self::NewChat),
            "settings" => Some(Self::Settings),
            "quit" | "exit" => Some(Self::Quit),
            other => other
                .strip_prefix("send:")
                .map(|body| Self::SendPrompt(body.replace("\\n", "\n"))),
        }
    }
}

pub fn socket_path() -> PathBuf {
    let dir = std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            let uid = unsafe { libc::getuid() };
            PathBuf::from(format!("/tmp/thetalk-{}", uid))
        });
    let _ = fs::create_dir_all(&dir);
    dir.join("thetalk.sock")
}

/// Attempts to send an IPC command to an already-running `thetalk` instance.
/// Returns `Ok(true)` if delivered to an active daemon, `Ok(false)` if no daemon is running.
pub fn try_send_to_running(cmd: &IpcCommand) -> Result<bool, String> {
    let path = socket_path();
    if !path.exists() {
        return Ok(false);
    }
    match UnixStream::connect(&path) {
        Ok(mut stream) => {
            let _ = stream.set_write_timeout(Some(Duration::from_millis(500)));
            stream
                .write_all(cmd.to_wire().as_bytes())
                .map_err(|e| e.to_string())?;
            let _ = stream.flush();
            Ok(true)
        }
        Err(_) => {
            // Stale socket from a killed process — safe to remove
            let _ = fs::remove_file(&path);
            Ok(false)
        }
    }
}

pub struct IpcServer {
    listener: UnixListener,
    path: PathBuf,
}

impl IpcServer {
    pub fn bind() -> Result<Self, String> {
        let path = socket_path();
        if path.exists() {
            let _ = fs::remove_file(&path);
        }
        let listener = UnixListener::bind(&path).map_err(|e| e.to_string())?;
        listener
            .set_nonblocking(true)
            .map_err(|e| e.to_string())?;
        Ok(Self { listener, path })
    }

    pub fn poll_commands(&self) -> Vec<IpcCommand> {
        let mut cmds = Vec::new();
        while let Ok((stream, _)) = self.listener.accept() {
            let _ = stream.set_read_timeout(Some(Duration::from_millis(50)));
            let reader = BufReader::new(stream);
            for line in reader.lines().map_while(Result::ok) {
                if let Some(cmd) = IpcCommand::from_wire(&line) {
                    cmds.push(cmd);
                }
            }
        }
        cmds
    }
}

impl Drop for IpcServer {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}
