//! The binary as a user or the service runs it: argument handling, exit codes,
//! and the engine's life cycle under signals. Every run gets scratch config and
//! runtime folders, a session bus address where nothing listens, and a device
//! path that does not exist, so no test reaches the user's own engine, profiles,
//! tray or keyboard.

use std::ffi::OsString;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::ffi::OsStringExt;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::thread::sleep;
use std::time::{Duration, Instant};

const NO_DEVICE: &str = "/nonexistent/loginux-cli-test";

/// A folder for one test that only this user can enter (mkdtemp picks a name
/// nobody has taken), removed when the test ends.
struct Scratch(PathBuf);

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn scratch() -> Scratch {
    let mut template = std::env::temp_dir().join("loginux-cli-XXXXXX").into_os_string().into_vec();
    template.push(0);
    // SAFETY: a writable, NUL-terminated template ending in XXXXXX, as mkdtemp requires.
    let made = unsafe { libc::mkdtemp(template.as_mut_ptr().cast()) };
    assert!(!made.is_null(), "mkdtemp: {}", std::io::Error::last_os_error());
    template.pop();
    let dir = PathBuf::from(OsString::from_vec(template));
    std::fs::create_dir(dir.join("run")).unwrap();
    Scratch(dir)
}

fn loginux(dir: &Path, args: &[&str]) -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_loginux"));
    cmd.args(args)
        .env("XDG_CONFIG_HOME", dir.join("config"))
        .env("XDG_RUNTIME_DIR", dir.join("run"))
        .env("DBUS_SESSION_BUS_ADDRESS", format!("unix:path={}", dir.join("run/bus").display()))
        .env_remove("INVOCATION_ID");
    cmd
}

fn output(dir: &Path, args: &[&str]) -> Output {
    loginux(dir, args).output().unwrap()
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

fn terminate(child: &Child) {
    // SAFETY: a signal to our own child process.
    unsafe { libc::kill(child.id() as libc::pid_t, libc::SIGTERM) };
}

/// The child's exit, or a panic after 15 seconds.
fn exited(child: &mut Child) -> std::process::ExitStatus {
    let end = Instant::now() + Duration::from_secs(15);
    loop {
        if let Some(status) = child.try_wait().unwrap() {
            return status;
        }
        assert!(Instant::now() < end, "loginux did not exit");
        sleep(Duration::from_millis(20));
    }
}

#[test]
fn help_errors_and_exit_codes() {
    let scratch = scratch();
    let dir = &scratch.0;
    let help = output(dir, &["--help"]);
    assert!(help.status.success() && text(&help.stdout).contains("Usage:"));
    let unknown = output(dir, &["bogus"]);
    assert_eq!(unknown.status.code(), Some(1));
    assert!(text(&unknown.stderr).starts_with("loginux: unknown command \"bogus\""));
    let zones = output(dir, &["zones"]);
    assert!(zones.status.success() && text(&zones.stdout).contains("zone"));
    let status = output(dir, &["status", "--device", NO_DEVICE]);
    assert_eq!(status.status.code(), Some(1));
    assert!(text(&status.stderr).contains(&format!("cannot open {NO_DEVICE}")), "{}", text(&status.stderr));
}

#[test]
fn the_engine_serves_until_a_signal_and_a_second_one_waits() {
    let scratch = scratch();
    let dir = &scratch.0;
    let sock = dir.join("run/loginux/sock");
    let mut first = loginux(dir, &["run", "--device", NO_DEVICE]).stderr(Stdio::null()).spawn().unwrap();
    let end = Instant::now() + Duration::from_secs(5);
    while !sock.exists() {
        assert!(Instant::now() < end, "no engine socket");
        sleep(Duration::from_millis(20));
    }
    let stream = UnixStream::connect(&sock).unwrap();
    (&stream).write_all(b"\"status\"\n").unwrap();
    let mut reply = String::new();
    BufReader::new(&stream).read_line(&mut reply).unwrap();
    assert!(reply.starts_with("{\"ok\":"), "{reply}");

    // A second engine waits for the lock; a signal ends the wait.
    let mut second = waiting(dir);
    terminate(&second);
    assert!(exited(&mut second).success());

    // A third one waits too, and takes over when the first stops.
    let mut third = waiting(dir);
    terminate(&first);
    assert!(exited(&mut first).success());
    let end = Instant::now() + Duration::from_secs(5);
    while UnixStream::connect(&sock).is_err() {
        assert!(Instant::now() < end, "the waiting engine did not take over");
        sleep(Duration::from_millis(20));
    }
    terminate(&third);
    assert!(exited(&mut third).success());
    assert!(!sock.exists(), "the socket goes with the engine");
}

/// An engine started while another holds the lock, once it waits in flock.
/// Its log is read to the end, so it never writes into a closed pipe.
fn waiting(dir: &Path) -> Child {
    let mut child = loginux(dir, &["run", "--device", NO_DEVICE]).stderr(Stdio::piped()).spawn().unwrap();
    let mut log = BufReader::new(child.stderr.take().unwrap()).lines();
    let said = log.next().unwrap().unwrap();
    assert!(said.contains("waiting for it to exit"), "{said}");
    std::thread::spawn(move || log.for_each(drop));
    sleep(Duration::from_millis(200)); // inside flock by now
    child
}

/// `loginux claude --hook` with `json` on stdin, as Claude Code 2.1.309 runs it
/// in the process 4242.
fn hook(dir: &Path, entrypoint: &str, json: &str) -> Output {
    let mut child = loginux(dir, &["claude", "--hook"])
        .env("CLAUDE_CODE_ENTRYPOINT", entrypoint)
        .env("CLAUDE_PID", "4242")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(json.as_bytes()).unwrap();
    child.wait_with_output().unwrap()
}

#[test]
fn a_claude_code_hook_never_fails_or_writes_to_stdout() {
    let scratch = scratch();
    let dir = &scratch.0;
    let prompt = r#"{"session_id":"s1","hook_event_name":"UserPromptSubmit","prompt":"hi"}"#;
    // No engine: nothing to show. Claude Code would read stdout as part of the prompt.
    let out = hook(dir, "cli", prompt);
    assert!(out.status.success() && out.stdout.is_empty(), "{out:?}");
    assert!(text(&out.stderr).contains("no loginux engine"), "{}", text(&out.stderr));
    let headless = hook(dir, "sdk-cli", prompt);
    assert!(headless.status.success() && headless.stderr.is_empty(), "a `claude -p` run is not shown");
    // An engine gets the instance, its process and the signal.
    std::fs::create_dir_all(dir.join("run/loginux")).unwrap();
    let listener = std::os::unix::net::UnixListener::bind(dir.join("run/loginux/sock")).unwrap();
    let engine = std::thread::spawn(move || {
        let (stream, _) = listener.accept().unwrap();
        let mut line = String::new();
        BufReader::new(&stream).read_line(&mut line).unwrap();
        let status = r#"{"ok":{"device":{"state":"missing"},"control":{"state":"onboard"},"active":null,"message":null,"pid":1,"hosted":false}}"#;
        (&stream).write_all(format!("{status}\n").as_bytes()).unwrap();
        line
    });
    let out = hook(dir, "cli", prompt);
    assert!(out.status.success() && out.stdout.is_empty() && out.stderr.is_empty(), "{out:?}");
    assert_eq!(engine.join().unwrap(), "{\"claude\":{\"session\":\"4242\",\"signal\":\"prompt\",\"pid\":4242}}\n");
}

#[test]
fn the_status_line_passes_through_to_the_users_own() {
    let scratch = scratch();
    let dir = &scratch.0;
    let json = r#"{"session_id":"s1","context_window":{"used_percentage":42},"rate_limits":{"five_hour":{"used_percentage":81,"resets_at":1}}}"#;
    let line = |args: &[&str]| {
        let mut child = loginux(dir, args)
            .env("CLAUDE_CODE_ENTRYPOINT", "cli")
            .env("CLAUDE_PID", "4242")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        child.stdin.take().unwrap().write_all(json.as_bytes()).unwrap();
        child.wait_with_output().unwrap()
    };
    // The user's own status line gets the same JSON, and what it prints is the status line.
    let out = line(&["claude", "--statusline", "--", "sh", "-c", "grep -c context_window"]);
    assert!(out.status.success(), "{out:?}");
    assert_eq!(text(&out.stdout), "1\n");
    assert!(text(&out.stderr).contains("no loginux engine"), "and the engine was tried");
    let missing = line(&["claude", "--statusline", "--", "/nonexistent/statusline"]);
    assert!(missing.status.success() && text(&missing.stderr).contains("/nonexistent/statusline"));
    // An engine gets the instance's context and the limits.
    std::fs::create_dir_all(dir.join("run/loginux")).unwrap();
    let listener = std::os::unix::net::UnixListener::bind(dir.join("run/loginux/sock")).unwrap();
    let engine = std::thread::spawn(move || {
        let (stream, _) = listener.accept().unwrap();
        let mut line = String::new();
        BufReader::new(&stream).read_line(&mut line).unwrap();
        let status = r#"{"ok":{"device":{"state":"missing"},"control":{"state":"onboard"},"active":null,"message":null,"pid":1,"hosted":false}}"#;
        (&stream).write_all(format!("{status}\n").as_bytes()).unwrap();
        line
    });
    let quiet = line(&["claude", "--statusline"]);
    assert!(quiet.status.success() && quiet.stdout.is_empty(), "{quiet:?}");
    let request = engine.join().unwrap();
    assert!(request.starts_with(r#"{"claude-usage":{"session":"4242","usage":{"context":42.0,"limits":true,"five_hour":{"used":81.0,"resets_at":1}"#), "{request}");
}
