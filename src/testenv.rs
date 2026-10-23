//! Shared test setup. `init` points the config and runtime folders at one
//! private scratch folder for the whole run, and every lookup of those folders,
//! of the device definitions and of sysfs calls it first in a test build, so no
//! test reads or writes the real ~/.config/loginux or sees the machine's own
//! devices. The session bus address points into the scratch folder as well,
//! where nothing listens unless a test starts its own bus, and `systemctl` is a
//! stand-in script (tests/stand-ins) that never reaches systemd.

use std::ffi::OsString;
use std::os::unix::ffi::OsStringExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Mutex, MutexGuard, Once, OnceLock};
use std::time::{Duration, Instant};

static INIT: Once = Once::new();
static ROOT: OnceLock<PathBuf> = OnceLock::new();
static SERIAL: Mutex<()> = Mutex::new(());

/// The scratch folder, with XDG_CONFIG_HOME and XDG_RUNTIME_DIR pointing into it.
pub fn init() -> &'static Path {
    INIT.call_once(|| {
        let root = scratch_folder();
        let devices = root.join("config/loginux/devices");
        std::fs::create_dir_all(&devices).expect("scratch config folder");
        std::fs::create_dir_all(root.join("run")).expect("scratch runtime folder");
        // A user definition, a broken one and a stray file, so loading them is tested.
        let pad = "name = \"Test Pad\"\nprotocol = \"hidpp20\"\nlighting = \"clusters\"\nvendor = 0x046d\n\
                   product = 0xffff\ninterface = 2\nboard = [2, 2]\n\
                   lights = [{ name = \"glow\", zone = 0, at = [0, 0] }]\n";
        std::fs::write(devices.join("test-pad.toml"), pad).expect("test definition");
        std::fs::write(devices.join("broken.toml"), "name = ").expect("broken definition");
        std::fs::write(devices.join("notes.txt"), "not a definition").expect("stray file");
        // sysfs: a G316's key-report interface, which is not the HID++ one, and
        // /dev/null (character device 1:3) passing for a G316's HID++ interface.
        let sysfs = root.join("sysfs");
        hid_node(&sysfs.join("class/hidraw/hidraw-fake0"), "0003:0000046D:0000C36B", "01");
        hid_node(&sysfs.join("dev/char/1:3"), "0003:0000046D:0000C36B", "02");
        crate::hidpp::use_sysfs(sysfs);
        std::fs::create_dir_all(root.join("systemd")).expect("the stand-in systemctl's folder");
        crate::ui::use_systemctl(stand_in_program("systemctl"));
        // SAFETY: this runs once, inside call_once, before any test reads these
        // variables: every test that reaches them calls init() first and waits here.
        unsafe {
            std::env::set_var("XDG_CONFIG_HOME", root.join("config"));
            std::env::set_var("XDG_RUNTIME_DIR", root.join("run"));
            std::env::set_var("DBUS_SESSION_BUS_ADDRESS", format!("unix:path={}", root.join("run/bus").display()));
            std::env::set_var("LOGINUX_TEST_SYSTEMD", root.join("systemd"));
            libc::atexit(remove_scratch_folder);
        }
        ROOT.set(root).expect("set once");
    });
    ROOT.get().expect("initialised")
}

/// A new folder under the temporary folder ($TMPDIR, else /tmp) that only this
/// user can enter. mkdtemp picks a name nobody has taken, so no earlier run
/// and no other user can have put anything in it.
fn scratch_folder() -> PathBuf {
    let mut template = std::env::temp_dir().join("loginux-tests-XXXXXX").into_os_string().into_vec();
    template.push(0);
    // SAFETY: a writable, NUL-terminated template ending in XXXXXX, as mkdtemp requires.
    let made = unsafe { libc::mkdtemp(template.as_mut_ptr().cast()) };
    assert!(!made.is_null(), "mkdtemp: {}", std::io::Error::last_os_error());
    template.pop();
    PathBuf::from(OsString::from_vec(template))
}

extern "C" fn remove_scratch_folder() {
    if let Some(root) = ROOT.get() {
        let _ = std::fs::remove_dir_all(root);
    }
}

/// A stand-in program from tests/stand-ins, run from the source tree: tests
/// never write a program and then run it.
pub fn stand_in_program(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/stand-ins").join(name)
}

/// Makes `class` a hidraw class directory as the kernel lays one out: its
/// `device` link leads to the HID device, whose uevent carries the
/// bus:vendor:product ID, and the USB interface number sits one level up.
pub fn hid_node(class: &Path, hid_id: &str, interface: &str) {
    let usb = class.join("usb");
    let _ = std::fs::remove_dir_all(class);
    std::fs::create_dir_all(usb.join("hid")).expect("scratch sysfs");
    std::fs::write(usb.join("hid/uevent"), format!("HID_ID={hid_id}\nHID_NAME=Logitech G316 X 98\n")).expect("uevent");
    std::fs::write(usb.join("bInterfaceNumber"), format!("{interface}\n")).expect("interface number");
    std::os::unix::fs::symlink("usb/hid", class.join("device")).expect("device link");
}

/// Interrupts `thread`'s blocking system call: sends it SIGUSR1, whose handler
/// does nothing, so the call returns EINTR.
pub fn interrupt(thread: libc::pthread_t) {
    static HANDLER: Once = Once::new();
    HANDLER.call_once(|| {
        extern "C" fn nothing(_: libc::c_int) {}
        // SAFETY: a handler that touches nothing; no SA_RESTART, so blocked calls return EINTR.
        unsafe {
            let mut sa: libc::sigaction = std::mem::zeroed();
            sa.sa_sigaction = nothing as extern "C" fn(libc::c_int) as libc::sighandler_t;
            libc::sigemptyset(&mut sa.sa_mask);
            libc::sigaction(libc::SIGUSR1, &sa, std::ptr::null_mut());
        }
    });
    // SAFETY: `thread` is a live thread of this process, by the caller's contract.
    unsafe { libc::pthread_kill(thread, libc::SIGUSR1) };
}

/// The stand-in systemctl's folder, where its `state`, `fails` and `log` files are.
pub fn systemd() -> PathBuf {
    init().join("systemd")
}

/// A private session bus on the scratch address, stopped on drop.
pub struct Bus(Child);

pub fn session_bus() -> Bus {
    let path = init().join("run/bus");
    let _ = std::fs::remove_file(&path);
    let daemon = Command::new("dbus-daemon")
        .args(["--session", "--nofork", "--nopidfile"])
        .arg(format!("--address=unix:path={}", path.display()))
        .stdout(Stdio::null())
        .spawn()
        .expect("dbus-daemon (package dbus) runs the tray test");
    let end = Instant::now() + Duration::from_secs(5);
    while !path.exists() {
        assert!(Instant::now() < end, "dbus-daemon did not start");
        std::thread::sleep(Duration::from_millis(10));
    }
    Bus(daemon)
}

impl Bus {
    /// The names on the bus, as `dbus-send` prints them.
    pub fn names(&self) -> String {
        let out = Command::new("dbus-send")
            .args(["--session", "--print-reply", "--dest=org.freedesktop.DBus", "/org/freedesktop/DBus"])
            .arg("org.freedesktop.DBus.ListNames")
            .output()
            .expect("dbus-send");
        String::from_utf8_lossy(&out.stdout).into_owned()
    }
}

impl Drop for Bus {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// A stand-in engine on the scratch socket: answers each request (one
/// connection each, as `ipc::request` makes them) with the next of `replies`,
/// then removes the socket. Joining gives the requests it saw.
pub fn stand_in(replies: Vec<crate::ipc::Reply>) -> std::thread::JoinHandle<Vec<crate::ipc::Request>> {
    use std::io::{BufRead, BufReader, Write};
    let path = crate::ipc::socket_path().expect("scratch runtime folder");
    let _ = std::fs::remove_file(&path);
    let listener = std::os::unix::net::UnixListener::bind(&path).expect("scratch socket");
    std::thread::spawn(move || {
        let mut seen = vec![];
        for reply in replies {
            let (stream, _) = listener.accept().expect("a client");
            let mut line = String::new();
            BufReader::new(&stream).read_line(&mut line).expect("a request");
            seen.push(serde_json::from_str(&line).expect("a valid request"));
            let out = serde_json::to_string(&reply).expect("a reply") + "\n";
            (&stream).write_all(out.as_bytes()).expect("the reply sent");
        }
        let _ = std::fs::remove_file(&path);
        seen
    })
}

/// Puts back the default actions of the signals the engine catches, after a
/// test ran code that installs its handlers.
pub fn default_signals() {
    for sig in [libc::SIGTERM, libc::SIGINT, libc::SIGHUP] {
        // SAFETY: restores a default disposition.
        unsafe { libc::signal(sig, libc::SIG_DFL) };
    }
}

/// Held by tests that share one file: config.toml, the engine socket or its lock.
pub fn serial() -> MutexGuard<'static, ()> {
    init();
    SERIAL.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}
