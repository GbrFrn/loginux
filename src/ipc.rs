//! The local socket between the engine and its clients (the editor window and
//! the command line): one JSON request per line, one JSON reply per line. The
//! socket sits in $XDG_RUNTIME_DIR/loginux, a directory only this user can enter.

use crate::profile::Profile;
use serde::{Deserialize, Serialize};
use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::time::Duration;

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "kebab-case")]
pub enum Request {
    Status,
    /// Choose the profile to keep applied; None hands the lighting back to the keyboard.
    Use { profile: Option<String> },
    /// Show an unsaved profile while the editor is open; it lapses unless renewed.
    Preview { profile: Option<Profile> },
    /// Light one zone alone for a channel check; it lapses unless renewed.
    Check { zone: Option<u8> },
    /// Take the lighting from another program that holds it.
    TakeOver,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "kebab-case")]
pub enum Device {
    Missing,
    NoAccess { error: String },
    Connected { keyboard: String, name: String, path: String, protocol: String, layout: Option<u8>, zones: Vec<u8> },
    Failed { error: String },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "kebab-case")]
pub enum Control {
    /// The keyboard runs its onboard lighting.
    Onboard,
    /// loginux wants to paint but the keyboard is not connected.
    Waiting,
    Profile { name: String },
    Preview,
    Check { zone: u8 },
    /// Another program holds the lighting; loginux waits for a take-over request.
    Conflict { mode: u8, flags: u8 },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Status {
    pub device: Device,
    pub control: Control,
    pub active: Option<String>,
    pub message: Option<String>,
    pub pid: u32,
    /// The engine runs inside an editor window and stops when it closes.
    pub hosted: bool,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Reply {
    Ok(Status),
    Err(String),
}

/// $XDG_RUNTIME_DIR/loginux, created private to this user.
pub fn runtime_dir() -> Result<PathBuf, String> {
    let base = std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        // SAFETY: getuid has no failure mode.
        .unwrap_or_else(|| PathBuf::from(format!("/run/user/{}", unsafe { libc::getuid() })));
    let dir = base.join("loginux");
    fs::create_dir_all(&dir)
        .and_then(|_| fs::set_permissions(&dir, fs::Permissions::from_mode(0o700)))
        .map_err(|e| format!("{}: {e}", dir.display()))?;
    Ok(dir)
}

pub fn socket_path() -> Result<PathBuf, String> {
    Ok(runtime_dir()?.join("sock"))
}

/// One request to the running engine. Err when no engine answers.
pub fn request(req: &Request) -> Result<Status, String> {
    let path = socket_path()?;
    let mut stream = UnixStream::connect(&path).map_err(|e| format!("no loginux engine at {}: {e}", path.display()))?;
    let timeout = Some(Duration::from_secs(5));
    stream.set_read_timeout(timeout).and_then(|_| stream.set_write_timeout(timeout)).map_err(|e| e.to_string())?;
    let mut line = serde_json::to_string(req).map_err(|e| e.to_string())?;
    line.push('\n');
    stream.write_all(line.as_bytes()).map_err(|e| format!("engine socket: {e}"))?;
    line.clear();
    BufReader::new(&stream).read_line(&mut line).map_err(|e| format!("engine socket: {e}"))?;
    match serde_json::from_str(&line).map_err(|e| format!("bad reply from engine: {e}"))? {
        Reply::Ok(status) => Ok(status),
        Reply::Err(e) => Err(e),
    }
}
