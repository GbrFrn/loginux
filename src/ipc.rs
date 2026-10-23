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

/// Externally tagged on purpose: an internally tagged enum makes serde buffer the
/// body, and the buffered path cannot read the zone map's numeric keys.
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
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
    /// Change the report rate. Sent only on an explicit user action; the keyboard
    /// can re-enumerate, and the engine reads the rate back after it returns.
    SetReportRate { hz: u32 },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "kebab-case")]
pub enum Device {
    Missing,
    NoAccess { error: String },
    Connected {
        keyboard: String,
        name: String,
        path: String,
        protocol: String,
        layout: Option<u8>,
        zones: Vec<u8>,
        /// Built-in effects the device can run: breathe, cycle, wave, ripple.
        #[serde(default)]
        effects: Vec<String>,
    },
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

/// Optional device settings, read from the keyboard; None when it does not report one.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Controls {
    /// Current and maximum lighting brightness (0x8040), read only.
    pub brightness: Option<[u16; 2]>,
    /// Current report rate in Hz (0x8061).
    pub report_rate: Option<u32>,
    /// Report rates the keyboard says it supports, in Hz.
    pub report_rates: Vec<u32>,
    /// Game mode (0x4523), read only: it changes with the keyboard's own key.
    pub game_mode: Option<bool>,
}

/// The last report-rate change the user asked for.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum RateChange {
    /// Sent; waiting for the keyboard to come back and for a readback.
    Waiting { hz: u32 },
    /// Read back after the change.
    Done { hz: u32 },
    Failed { hz: u32, error: String },
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
    /// Present while a keyboard is connected.
    #[serde(default)]
    pub controls: Option<Controls>,
    #[serde(default)]
    pub rate_change: Option<RateChange>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
#[allow(clippy::large_enum_variant)] // one reply per request; the size does not matter
pub enum Reply {
    Ok(Status),
    Err(String),
}

/// $XDG_RUNTIME_DIR/loginux, created private to this user.
pub fn runtime_dir() -> Result<PathBuf, String> {
    #[cfg(test)]
    crate::testenv::init();
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

#[derive(Debug)]
pub enum Error {
    /// No engine answered: none is running, or the socket broke.
    Unreachable(String),
    /// The engine answered and refused the request.
    Refused(String),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        match self {
            Error::Unreachable(e) | Error::Refused(e) => f.write_str(e),
        }
    }
}

impl From<Error> for String {
    fn from(e: Error) -> String {
        e.to_string()
    }
}

/// One request to the running engine.
pub fn request(req: &Request) -> Result<Status, Error> {
    let gone = |e: String| Error::Unreachable(e);
    let path = socket_path().map_err(gone)?;
    let mut stream =
        UnixStream::connect(&path).map_err(|e| gone(format!("no loginux engine at {}: {e}", path.display())))?;
    let timeout = Some(Duration::from_secs(5));
    stream.set_read_timeout(timeout).and_then(|_| stream.set_write_timeout(timeout)).map_err(|e| gone(e.to_string()))?;
    let mut line = serde_json::to_string(req).map_err(|e| Error::Refused(e.to_string()))?;
    line.push('\n');
    stream.write_all(line.as_bytes()).map_err(|e| gone(format!("engine socket: {e}")))?;
    line.clear();
    BufReader::new(&stream).read_line(&mut line).map_err(|e| gone(format!("engine socket: {e}")))?;
    match serde_json::from_str(&line).map_err(|e| gone(format!("bad reply from engine: {e}")))? {
        Reply::Ok(status) => Ok(status),
        Reply::Err(e) => Err(Error::Refused(e)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufRead, BufReader, Write};
    use std::os::unix::net::UnixListener;
    use std::thread;

    fn status() -> Status {
        Status {
            device: Device::Missing,
            control: Control::Onboard,
            active: Some("Wave".into()),
            message: None,
            pid: 7,
            hosted: false,
            controls: None,
            rate_change: None,
        }
    }

    #[test]
    fn preview_with_zone_keys_round_trips() {
        let mut p = Profile::default();
        p.zones.insert(1, [1, 2, 3]);
        let line = serde_json::to_string(&Request::Preview { profile: Some(p.clone()) }).unwrap();
        match serde_json::from_str::<Request>(&line).unwrap() {
            Request::Preview { profile: Some(q) } => assert_eq!(q, p),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn every_request_round_trips() {
        for req in [
            Request::Status,
            Request::Use { profile: Some("Wave".into()) },
            Request::Use { profile: None },
            Request::Preview { profile: None },
            Request::Check { zone: Some(38) },
            Request::TakeOver,
            Request::SetReportRate { hz: 8000 },
        ] {
            let line = serde_json::to_string(&req).unwrap();
            let back: Request = serde_json::from_str(&line).unwrap();
            assert_eq!(serde_json::to_string(&back).unwrap(), line);
        }
        assert_eq!(serde_json::to_string(&Request::Use { profile: None }).unwrap(), r#"{"use":{"profile":null}}"#);
    }

    #[test]
    fn a_status_from_an_older_engine_still_parses() {
        let old = r#"{"device":{"state":"connected","keyboard":"g316-x-98","name":"G316","path":"/dev/hidraw2","protocol":"4.2","layout":1,"zones":[38]},
                      "control":{"state":"profile","name":"Test"},"active":"Test","message":null,"pid":1,"hosted":false}"#;
        let s: Status = serde_json::from_str(old).unwrap();
        assert!(matches!(s.device, Device::Connected { ref effects, .. } if effects.is_empty()));
        assert!(s.controls.is_none() && s.rate_change.is_none());
        let reply = serde_json::to_string(&Reply::Err("no".into())).unwrap();
        assert!(matches!(serde_json::from_str::<Reply>(&reply).unwrap(), Reply::Err(e) if e == "no"));
    }

    #[test]
    fn errors_print_their_message() {
        assert_eq!(Error::Unreachable("down".into()).to_string(), "down");
        assert_eq!(String::from(Error::Refused("no".into())), "no");
    }

    #[test]
    fn the_runtime_folder_is_private() {
        let root = crate::testenv::init();
        let dir = runtime_dir().unwrap();
        assert_eq!(dir, root.join("run/loginux"));
        assert_eq!(fs::metadata(&dir).unwrap().permissions().mode() & 0o777, 0o700);
        assert_eq!(socket_path().unwrap(), dir.join("sock"));
    }

    /// Answers one connection on the engine socket with `reply`, returning the request line.
    fn serve_once(reply: &'static str) -> thread::JoinHandle<String> {
        let path = socket_path().unwrap();
        let _ = fs::remove_file(&path);
        let listener = UnixListener::bind(&path).unwrap();
        thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            let mut line = String::new();
            BufReader::new(&stream).read_line(&mut line).unwrap();
            (&stream).write_all(reply.as_bytes()).unwrap();
            line
        })
    }

    #[test]
    fn requests_over_the_socket() {
        let _serial = crate::testenv::serial();
        let ok = format!("{}\n", serde_json::to_string(&Reply::Ok(status())).unwrap()).leak();
        let server = serve_once(ok);
        assert_eq!(request(&Request::Status).unwrap().pid, 7);
        assert_eq!(server.join().unwrap(), "\"status\"\n");

        let server = serve_once("{\"err\":\"refused here\"}\n");
        assert!(matches!(request(&Request::TakeOver), Err(Error::Refused(e)) if e == "refused here"));
        server.join().unwrap();

        let server = serve_once("garbage\n");
        assert!(matches!(request(&Request::Status), Err(Error::Unreachable(e)) if e.contains("bad reply")));
        server.join().unwrap();

        fs::remove_file(socket_path().unwrap()).unwrap();
        assert!(matches!(request(&Request::Status), Err(Error::Unreachable(e)) if e.contains("no loginux engine")));
    }
}
