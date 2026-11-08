use std::fs;
use std::io::{Read, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::time::{Duration, Instant};

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
    pending: Vec<PendingClient>,
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
        Ok(Self { listener, path, pending: Vec::new() })
    }

    /// Never wait on a sender in the UI loop. Bound peers, bytes and accepts
    /// per tick so a partial/slow IPC command cannot stall caret or animation.
    pub fn poll_commands(&mut self) -> Vec<IpcCommand> {
        for _ in 0..32 {
            let Ok((stream, _)) = self.listener.accept() else { break; };
            if self.pending.len() < 32 && stream.set_nonblocking(true).is_ok() {
                self.pending.push(PendingClient { stream, data: Vec::new(), started: Instant::now() });
            }
        }
        let mut cmds = Vec::new();
        self.pending.retain_mut(|peer| peer.poll(&mut cmds));
        cmds
    }

}

struct PendingClient {
    stream: UnixStream,
    data: Vec<u8>,
    started: Instant,
}

impl PendingClient {
    fn poll(&mut self, commands: &mut Vec<IpcCommand>) -> bool {
        if self.started.elapsed() > Duration::from_secs(2) { return false; }
        let mut buf = [0_u8; 4096];
        let eof = match self.stream.read(&mut buf) {
            Ok(0) => true,
            Ok(n) => { self.data.extend_from_slice(&buf[..n]); false }
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => false,
            Err(_) => return false,
        };
        if self.data.len() > 64 * 1024 { return false; }
        let mut consumed = 0;
        for (i, byte) in self.data.iter().enumerate() {
            if *byte == b'\n' {
                if let Ok(line) = std::str::from_utf8(&self.data[consumed..i]) {
                    if let Some(cmd) = IpcCommand::from_wire(line) { commands.push(cmd); }
                }
                consumed = i + 1;
            }
        }
        self.data.drain(..consumed);
        !eof
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn partial_ipc_does_not_wait_and_survives_across_ticks() {
        let (mut writer, reader) = UnixStream::pair().unwrap();
        reader.set_nonblocking(true).unwrap();
        let mut peer = PendingClient { stream: reader, data: Vec::new(), started: Instant::now() };
        let mut cmds = Vec::new();
        assert!(peer.poll(&mut cmds));
        writer.write_all(b"sho").unwrap();
        assert!(peer.poll(&mut cmds));
        assert!(cmds.is_empty());
        writer.write_all(b"w\n").unwrap();
        assert!(peer.poll(&mut cmds));
        assert_eq!(cmds, vec![IpcCommand::Show]);
    }
}

impl Drop for IpcServer {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}
