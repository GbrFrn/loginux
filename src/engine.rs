//! The engine: the one process that opens the keyboard. It keeps the chosen
//! profile applied through replugs and USB re-enumeration, serves the editor and
//! the command line over the local socket, and hands the lighting back to the
//! keyboard when it stops.
//!
//! Ownership of the lighting is recorded twice: on the keyboard (0x8071 software
//! control, mode bit 0) and in $XDG_RUNTIME_DIR/loginux/claim, which holds the
//! values to restore. A claim on the keyboard without that file belongs to some
//! other program, and loginux waits for an explicit take-over instead of
//! overwriting it.

use crate::device::{Event, Keyboard, RATES_HZ};
use crate::hidpp::{self, Error};
use crate::ipc::{self, Control, Controls, Device, RateChange, Reply, Request, Status};
use crate::layout::{self, Kind, Model};
use crate::profile::{self, Profile, Rgb};
use std::fs::{self, File, OpenOptions};
use std::io::{self, BufRead, BufReader, Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError, Sender};
use std::sync::Arc;
use std::thread::{self, sleep};
use std::time::{Duration, Instant, SystemTime};

/// Set by SIGTERM, SIGINT and SIGHUP.
pub static STOP: AtomicBool = AtomicBool::new(false);

const TICK: Duration = Duration::from_millis(250);
const CHECK_EVERY: Duration = Duration::from_secs(2);
const LEASE: Duration = Duration::from_secs(10);
const SETTLE: Duration = Duration::from_millis(50);
const CONTROLS_EVERY: Duration = Duration::from_secs(10);
/// A rate change that did not re-enumerate is read back after this long. On
/// Windows the keyboard left the bus at the change itself (G HUB capture); the
/// margin covers a slower drop, which has not been observed on Linux.
const RATE_READBACK: Duration = Duration::from_secs(5);
/// After this long without the keyboard, a rate change is reported as failed.
const RATE_GIVE_UP: Duration = Duration::from_secs(20);

pub fn install_signal_handlers() {
    extern "C" fn on_signal(_: libc::c_int) {
        STOP.store(true, Ordering::SeqCst);
    }
    for sig in [libc::SIGTERM, libc::SIGINT, libc::SIGHUP] {
        // SAFETY: the handler only stores to an atomic. No SA_RESTART, so a blocked
        // flock or sleep returns EINTR and the caller sees STOP.
        unsafe {
            let mut sa: libc::sigaction = std::mem::zeroed();
            sa.sa_sigaction = on_signal as extern "C" fn(libc::c_int) as libc::sighandler_t;
            libc::sigemptyset(&mut sa.sa_mask);
            libc::sigaction(sig, &sa, std::ptr::null_mut());
        }
    }
}

/// Exclusive right to open the keyboard, held for the life of the owner.
pub struct Lock(#[allow(dead_code)] File);

/// Ok(None) when another loginux holds the lock (or a signal ended the wait).
pub fn lock(wait: bool) -> Result<Option<Lock>, String> {
    let path = ipc::runtime_dir()?.join("lock");
    let mut file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .mode(0o600)
        .open(&path)
        .map_err(|e| format!("{}: {e}", path.display()))?;
    loop {
        let op = libc::LOCK_EX | if wait { 0 } else { libc::LOCK_NB };
        // SAFETY: flock on an fd we own.
        if unsafe { libc::flock(file.as_raw_fd(), op) } == 0 {
            let _ = file.set_len(0).and_then(|_| writeln!(file, "{}", std::process::id()));
            return Ok(Some(Lock(file)));
        }
        match io::Error::last_os_error().raw_os_error() {
            Some(libc::EWOULDBLOCK) => return Ok(None),
            Some(libc::EINTR) if !STOP.load(Ordering::SeqCst) => continue,
            Some(libc::EINTR) => return Ok(None),
            _ => return Err(format!("{}: {}", path.display(), io::Error::last_os_error())),
        }
    }
}

/// PID written by the current lock holder.
pub fn owner() -> Option<u32> {
    let mut text = String::new();
    File::open(ipc::runtime_dir().ok()?.join("lock")).ok()?.read_to_string(&mut text).ok()?;
    text.trim().parse().ok()
}

// ---- claim marker --------------------------------------------------------

/// The lighting is held by loginux: what to restore, and what the keyboard read
/// back after the claim.
pub struct Held {
    pub restore: (u8, u8),
    pub state: (u8, u8),
    base: Option<Rgb>,
    frame: Vec<(u8, Rgb)>,
}

pub enum TakeError {
    Conflict(u8, u8),
    Hid(Error),
}

impl From<Error> for TakeError {
    fn from(e: Error) -> Self {
        TakeError::Hid(e)
    }
}

fn marker() -> Option<PathBuf> {
    Some(ipc::runtime_dir().ok()?.join("claim"))
}

/// (values to restore, software control read back after the claim)
type Marker = ((u8, u8), Option<(u8, u8)>);

fn read_marker() -> Option<Marker> {
    let text = fs::read_to_string(marker()?).ok()?;
    let pair = |key: &str| -> Option<(u8, u8)> {
        let v = text.lines().find_map(|l| l.strip_prefix(key))?;
        let mut it = v.split_whitespace().map(|b| u8::from_str_radix(b, 16).ok());
        Some((it.next()??, it.next()??))
    };
    Some((pair("restore=")?, pair("held=")))
}

fn write_marker(restore: (u8, u8), held: Option<(u8, u8)>) {
    let mut text = format!("restore={:02x} {:02x}\n", restore.0, restore.1);
    if let Some((m, f)) = held {
        text += &format!("held={m:02x} {f:02x}\n");
    }
    if let Some(path) = marker() {
        let _ = fs::write(path, text);
    }
}

fn clear_marker() {
    if let Some(path) = marker() {
        let _ = fs::remove_file(path);
    }
}

/// Takes the lighting effects from the firmware (mode bit 0; the firmware keeps
/// power management). A claim left by an earlier loginux is adopted; anyone
/// else's is a conflict unless `force`, and a forced take-over restores plain
/// firmware control (0, 0) on release.
pub fn take(kb: &mut Keyboard, force: bool) -> Result<Held, TakeError> {
    let now = kb.sw_control()?;
    let restore = if now.0 & 1 == 0 {
        now
    } else {
        match read_marker() {
            Some((restore, held)) if held.is_none_or(|h| h == now) => restore,
            _ if force => (0, 0),
            _ => return Err(TakeError::Conflict(now.0, now.1)),
        }
    };
    write_marker(restore, None);
    kb.set_sw_control(1, 0)?;
    sleep(SETTLE); // let the LED task apply the claim before SetEffect
    let state = kb.sw_control()?;
    write_marker(restore, Some(state));
    Ok(Held { restore, state, base: None, frame: vec![] })
}

/// Static `base` on every cluster, then the frame twice: the first frame after a
/// claim can be dropped (Solaar replays it too).
pub fn paint(kb: &mut Keyboard, statics: &[(u8, u8)], base: Rgb, frame: &[(u8, Rgb)]) -> Result<(), Error> {
    for &s in statics {
        kb.set_static(s, base)?;
    }
    sleep(SETTLE);
    kb.send_frame(frame)?;
    kb.send_frame(frame)
}

/// Hands the effects back; the firmware then reloads its onboard effects.
/// Returns what the keyboard reports afterwards.
pub fn give_back(kb: &mut Keyboard, restore: (u8, u8)) -> Result<(u8, u8), Error> {
    kb.set_sw_control(restore.0 & !1, restore.1)?;
    clear_marker();
    kb.sw_control()
}

/// For `loginux release` without an engine: the saved values, or plain firmware control.
pub fn release_now(kb: &mut Keyboard) -> Result<(u8, u8), Error> {
    let restore = read_marker().map_or((0, 0), |(r, _)| r);
    give_back(kb, restore)
}

// ---- engine ----------------------------------------------------------------

/// Static base colour and the per-zone frame.
type Paint = (Rgb, Vec<(u8, Rgb)>);

pub struct Options {
    pub device: Option<PathBuf>,
    /// Resend the frame this often even when nothing changed (off by default).
    pub refresh: Option<Duration>,
}

struct Link {
    kb: Keyboard,
    zones: Vec<u8>,
    statics: Vec<(u8, u8)>,
    held: Option<Held>,
    conflict: Option<(u8, u8)>,
    next_check: Instant,
    next_refresh: Option<Instant>,
    controls: Controls,
    rates: Option<Rates>,
    next_controls: Instant,
}

/// A report-rate write the user asked for, waiting for a readback.
struct RatePending {
    index: u8,
    hz: u32,
    sent: Instant,
}

/// An optional read: a missing or failing feature is "unknown", only a lost device is an error.
fn optional<T>(read: Result<Option<T>, Error>) -> Result<Option<T>, Error> {
    match read {
        Err(e @ Error::Gone(_)) => Err(e),
        Err(_) => Ok(None),
        ok => ok,
    }
}

/// Supported 0x8061 rate indices and the current one.
pub type Rates = (Vec<u8>, u8);

pub fn read_controls(kb: &mut Keyboard) -> Result<(Controls, Option<Rates>), Error> {
    let brightness = optional(kb.brightness())?.map(|(now, max)| [now, max]);
    let rates = optional(kb.report_rates())?;
    let game_mode = optional(kb.game_mode())?;
    let hz = |i: u8| RATES_HZ.get(usize::from(i)).copied();
    let controls = Controls {
        brightness,
        report_rate: rates.as_ref().and_then(|(_, current)| hz(*current)),
        report_rates: rates.as_ref().map(|(supported, _)| supported.iter().filter_map(|&i| hz(i)).collect()).unwrap_or_default(),
        game_mode,
    };
    Ok((controls, rates))
}

struct Engine {
    opts: Options,
    /// The keyboard definition in use; replaced by whatever keyboard connects.
    model: &'static Model,
    hosted: bool,
    link: Option<Link>,
    device: Device,
    retry_at: Instant,
    active: Option<(String, Profile, Option<SystemTime>)>,
    next_reload: Instant,
    preview: Option<(Profile, Instant)>,
    check: Option<(u8, Instant)>,
    take_over: bool,
    message: Option<String>,
    rate_pending: Option<RatePending>,
    rate_change: Option<RateChange>,
}

fn log(msg: &str) {
    eprintln!("loginux: {msg}");
}

/// Runs until `stop`, a signal, or a fatal socket error. Holds `lock` throughout.
pub fn run(opts: Options, hosted: bool, stop: Arc<AtomicBool>, lock: Lock) -> Result<(), String> {
    let sock = ipc::socket_path()?;
    let _ = fs::remove_file(&sock); // we hold the lock, so any socket file is stale
    let listener = UnixListener::bind(&sock).map_err(|e| format!("{}: {e}", sock.display()))?;
    let _ = fs::set_permissions(&sock, fs::Permissions::from_mode(0o600));
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || serve(listener, tx));

    let now = Instant::now();
    let mut engine = Engine {
        opts,
        model: layout::current(),
        hosted,
        link: None,
        device: Device::Missing,
        retry_at: now,
        active: None,
        next_reload: now,
        preview: None,
        check: None,
        take_over: false,
        message: None,
        rate_pending: None,
        rate_change: None,
    };
    log(&format!("engine started (pid {})", std::process::id()));
    while !stop.load(Ordering::SeqCst) && !STOP.load(Ordering::SeqCst) {
        match rx.recv_timeout(TICK) {
            Ok((req, reply)) => {
                let answer = engine.handle(req);
                let _ = reply.send(answer);
            }
            Err(RecvTimeoutError::Timeout) => engine.tick(),
            Err(RecvTimeoutError::Disconnected) => break,
        }
    }
    engine.shutdown();
    let _ = fs::remove_file(&sock);
    drop(lock);
    Ok(())
}

type Job = (Request, Sender<Reply>);

fn serve(listener: UnixListener, tx: Sender<Job>) {
    for stream in listener.incoming().flatten() {
        let tx = tx.clone();
        thread::spawn(move || client(stream, tx));
    }
}

fn client(stream: UnixStream, tx: Sender<Job>) {
    let mut reader = BufReader::new(&stream);
    loop {
        let mut line = String::new();
        match reader.by_ref().take(1 << 20).read_line(&mut line) {
            Ok(n) if n > 0 && line.ends_with('\n') => {}
            _ => return,
        }
        let reply = match serde_json::from_str::<Request>(&line) {
            Ok(req) => {
                let (rtx, rrx) = mpsc::channel();
                if tx.send((req, rtx)).is_err() {
                    return;
                }
                rrx.recv_timeout(Duration::from_secs(10)).unwrap_or(Reply::Err("engine did not answer".into()))
            }
            Err(e) => Reply::Err(format!("bad request: {e}")),
        };
        let Ok(mut out) = serde_json::to_string(&reply) else { return };
        out.push('\n');
        if (&stream).write_all(out.as_bytes()).is_err() {
            return;
        }
    }
}

impl Engine {
    fn handle(&mut self, req: Request) -> Reply {
        let lease = Instant::now() + LEASE;
        let result = match req {
            Request::Status => {
                self.preview.iter_mut().for_each(|p| p.1 = lease);
                self.check.iter_mut().for_each(|c| c.1 = lease);
                Ok(())
            }
            Request::Use { profile } => self.choose(profile),
            Request::Preview { profile } => match profile.map(|p| p.check_zones(self.model).map(|_| p)).transpose() {
                Ok(p) => {
                    self.preview = p.map(|p| (p, lease));
                    Ok(())
                }
                Err(e) => Err(e),
            },
            Request::Check { zone } => match zone {
                Some(id) if self.model.zone(id).is_none() => Err(format!("zone {id} is not one of the {}'s zones", self.model.name)),
                _ => {
                    self.check = zone.map(|z| (z, lease));
                    Ok(())
                }
            },
            Request::TakeOver => {
                self.take_over = true;
                if let Some(link) = &mut self.link {
                    link.next_check = Instant::now();
                }
                Ok(())
            }
            Request::SetReportRate { hz } => self.set_report_rate(hz),
        };
        self.tick();
        match result {
            Ok(()) => Reply::Ok(self.status()),
            Err(e) => Reply::Err(e),
        }
    }

    /// The one device-setting write: sent once, at the user's request. The keyboard
    /// can re-enumerate before it replies, so completion is decided by a readback.
    fn set_report_rate(&mut self, hz: u32) -> Result<(), String> {
        if self.rate_pending.is_some() {
            return Err("a report-rate change is already waiting for the keyboard".into());
        }
        let link = self.link.as_mut().ok_or("the keyboard is not connected")?;
        let (supported, current) = link.rates.clone().ok_or("this keyboard does not report a report rate (0x8061)")?;
        let index = RATES_HZ
            .iter()
            .position(|&r| r == hz)
            .map(|i| i as u8)
            .filter(|i| supported.contains(i))
            .ok_or_else(|| format!("{hz} Hz is not a rate this keyboard reports"))?;
        if index == current {
            self.rate_change = Some(RateChange::Done { hz });
            return Ok(());
        }
        log(&format!("changing the report rate to {hz} Hz at the user's request; the keyboard may reconnect"));
        self.rate_change = Some(RateChange::Waiting { hz });
        match link.kb.set_report_rate(index) {
            // Answered, or gone before it could answer: decide by reading back.
            Ok(()) | Err(Error::Gone(_)) | Err(Error::Timeout(_)) => {
                self.rate_pending = Some(RatePending { index, hz, sent: Instant::now() });
                Ok(())
            }
            Err(e) => {
                let error = e.to_string();
                self.rate_change = Some(RateChange::Failed { hz, error: error.clone() });
                Err(error)
            }
        }
    }

    /// Settles a pending rate change against what the keyboard now reports.
    fn rate_readback(&mut self, current: Option<u8>) {
        let Some(p) = self.rate_pending.take() else { return };
        self.rate_change = Some(match current {
            Some(i) if i == p.index => {
                log(&format!("report rate read back as {} Hz", p.hz));
                RateChange::Done { hz: p.hz }
            }
            Some(i) => {
                let got = RATES_HZ.get(usize::from(i)).map_or_else(|| format!("index {i}"), |r| format!("{r} Hz"));
                log(&format!("report rate read back as {got}, not {} Hz", p.hz));
                RateChange::Failed { hz: p.hz, error: format!("the keyboard reports {got}") }
            }
            None => RateChange::Failed { hz: p.hz, error: "the keyboard did not report its rate after the change".into() },
        });
    }

    fn choose(&mut self, name: Option<String>) -> Result<(), String> {
        if let Some(name) = &name {
            profile::load(self.model, name)?;
        }
        profile::set_active(name.as_deref())?;
        self.preview = None;
        self.check = None;
        self.next_reload = Instant::now();
        self.reload();
        Ok(())
    }

    fn tick(&mut self) {
        let now = Instant::now();
        if now >= self.next_reload {
            self.reload();
        }
        if self.preview.as_ref().is_some_and(|p| now >= p.1) {
            self.preview = None;
        }
        if self.check.as_ref().is_some_and(|c| now >= c.1) {
            self.check = None;
        }
        if self.link.is_none() && now >= self.retry_at {
            self.connect();
        }
        if self.link.is_none() && self.rate_pending.as_ref().is_some_and(|p| now - p.sent > RATE_GIVE_UP) {
            let hz = self.rate_pending.take().map_or(0, |p| p.hz);
            let error = format!("the keyboard did not come back within {} s; Loginux keeps looking for it", RATE_GIVE_UP.as_secs());
            self.say(&format!("report-rate change to {hz} Hz: {error}"));
            self.rate_change = Some(RateChange::Failed { hz, error });
        }
        if self.link.is_some()
            && let Err(e) = self.step()
        {
            let gone = matches!(e, Error::Gone(_));
            if gone && self.rate_pending.is_some() {
                log("keyboard left the bus after the report-rate change; waiting for it to return");
            } else {
                self.say(&e.to_string());
            }
            self.link = None; // closes the node; a claim stays recorded in the marker
            self.device = if gone { Device::Missing } else { Device::Failed { error: e.to_string() } };
            self.retry_at = now + if gone { Duration::from_millis(500) } else { Duration::from_secs(5) };
        }
    }

    fn say(&mut self, msg: &str) {
        if self.message.as_deref() != Some(msg) {
            log(msg);
            self.message = Some(msg.into());
        }
    }

    /// Follows config.toml and the chosen profile's file, so hand edits apply.
    fn reload(&mut self) {
        self.next_reload = Instant::now() + Duration::from_secs(1);
        let Some(name) = profile::active() else {
            self.active = None;
            return;
        };
        let Ok(path) = profile::path(&name) else {
            self.active = None;
            return self.say(&format!("config.toml names an invalid profile {name:?}"));
        };
        let stamp = profile::modified(&path);
        if self.active.as_ref().is_some_and(|(n, _, s)| *n == name && *s == stamp) {
            return;
        }
        match profile::read(self.model, &path) {
            Ok(p) => {
                log(&format!("profile {name:?} loaded"));
                self.active = Some((name, p, stamp));
            }
            // Keep painting the last good version of the same profile while its file is broken.
            Err(e) => {
                if self.active.as_ref().is_some_and(|(n, ..)| *n == name) {
                    if let Some(a) = &mut self.active {
                        a.2 = stamp;
                    }
                } else {
                    self.active = None;
                }
                self.say(&e);
            }
        }
    }

    fn connect(&mut self) {
        let found = match self.opts.device.clone() {
            Some(path) => hidpp::identify(&path, layout::models()).map(|m| (path, m)),
            None => hidpp::find(layout::models()),
        };
        let result = found.and_then(|(path, model)| {
            let mut kb = Keyboard::open(&path, model)?;
            let protocol = kb.protocol()?;
            let layout = kb.layout_code()?;
            let zones = kb.zones()?;
            let statics = kb.static_effects()?;
            let (controls, rates) = read_controls(&mut kb)?;
            Ok((kb, protocol, layout, zones, statics, controls, rates))
        });
        let now = Instant::now();
        match result {
            Ok((kb, protocol, layout, zones, statics, controls, rates)) => {
                let path = kb.path.display().to_string();
                let model = kb.model;
                log(&format!("{} connected on {path}: HID++ {}.{}, {} zones", model.name, protocol.0, protocol.1, zones.len()));
                let missing = model.zones.iter().filter(|z| !zones.contains(&z.id)).count();
                if missing > 0 {
                    log(&format!("{missing} zones of the {} definition are not reported by this keyboard", model.name));
                }
                if model.id != self.model.id {
                    self.model = model;
                    self.active = None; // parse the chosen profile again for this keyboard
                    self.next_reload = now;
                }
                if profile::chosen_keyboard().as_deref() != Some(model.id.as_str()) {
                    let _ = profile::set_keyboard(&model.id);
                }
                self.device = Device::Connected {
                    keyboard: model.id.clone(),
                    name: model.name.clone(),
                    path,
                    protocol: format!("{}.{}", protocol.0, protocol.1),
                    layout,
                    zones: zones.clone(),
                };
                self.message = None;
                let current = rates.as_ref().map(|r| r.1);
                self.link = Some(Link {
                    kb,
                    zones,
                    statics,
                    held: None,
                    conflict: None,
                    next_check: now,
                    next_refresh: None,
                    controls,
                    rates,
                    next_controls: now + CONTROLS_EVERY,
                });
                if self.rate_pending.is_some() {
                    self.rate_readback(current);
                }
            }
            Err(e) => {
                self.device = match &e {
                    Error::NotFound => Device::Missing,
                    Error::Access(..) => Device::NoAccess { error: e.to_string() },
                    _ => Device::Failed { error: e.to_string() },
                };
                if !matches!(e, Error::NotFound) {
                    self.say(&e.to_string());
                }
                self.retry_at = now + if matches!(e, Error::NotFound) { Duration::from_secs(1) } else { Duration::from_secs(3) };
            }
        }
    }

    /// What the keyboard should show: (static base, frame), or None for onboard lighting.
    fn desired(&self, zones: &[u8]) -> Result<Option<Paint>, String> {
        if let Some((id, _)) = self.check {
            if !zones.contains(&id) {
                return Err(format!("zone {id} is not reported by this keyboard"));
            }
            // Every key and strip segment dark, the zone under check white.
            let model = self.model;
            let dark = zones.iter().filter(|&&z| model.zone(z).is_some_and(|z| z.kind != Kind::Unknown) && z != id);
            let frame = dark.map(|&z| (z, [0; 3])).chain([(id, [0xFF; 3])]).collect();
            return Ok(Some(([0; 3], frame)));
        }
        let shown = self.preview.as_ref().map(|p| &p.0).or(self.active.as_ref().map(|a| &a.1));
        shown.map(|p| Ok((p.base("keys"), p.frame(self.model, zones)?))).transpose()
    }

    fn step(&mut self) -> Result<(), Error> {
        let Some(zones) = self.link.as_ref().map(|l| l.zones.clone()) else { return Ok(()) };
        let desired = match self.desired(&zones) {
            Ok(d) => d,
            Err(e) => {
                self.say(&e);
                None
            }
        };
        let force = self.take_over;
        let refresh = self.opts.refresh;
        let now = Instant::now();
        {
            let link = self.link.as_mut().expect("checked above");
            link.kb.alive()?;
            for event in link.kb.events() {
                match event {
                    Event::GameMode(on) => link.controls.game_mode = Some(on),
                }
            }
            let readback_due = self.rate_pending.as_ref().is_some_and(|p| now - p.sent > RATE_READBACK);
            if now >= link.next_controls || readback_due {
                link.next_controls = now + CONTROLS_EVERY;
                let (controls, rates) = read_controls(&mut link.kb)?;
                let current = rates.as_ref().map(|r| r.1);
                (link.controls, link.rates) = (controls, rates);
                if readback_due {
                    self.rate_readback(current); // the keyboard kept its connection through the change
                }
            }
        }
        let link = self.link.as_mut().expect("checked above");

        let Some((base, frame)) = desired else {
            if let Some(h) = link.held.take() {
                let after = give_back(&mut link.kb, h.restore)?;
                log(&format!("lighting handed back to the keyboard (software control now {:02x} {:02x})", after.0, after.1));
            }
            link.conflict = None;
            link.next_check = now;
            return Ok(());
        };

        if now >= link.next_check {
            link.next_check = now + CHECK_EVERY;
            let state = link.kb.sw_control()?;
            if let Some(h) = &link.held
                && state != h.state
            {
                link.held = None;
                if state.0 & 1 == 0 {
                    log("the keyboard took its lighting back (reset or wake); applying again");
                } else {
                    log(&format!("another program took the lighting (software control {:02x} {:02x})", state.0, state.1));
                    clear_marker();
                    link.conflict = Some(state);
                }
            }
            if link.held.is_none() && (link.conflict.is_none() || force) {
                match take(&mut link.kb, force) {
                    Ok(h) => {
                        log(&format!("lighting taken (restore {:02x} {:02x} on release)", h.restore.0, h.restore.1));
                        link.held = Some(h);
                        link.conflict = None;
                        self.take_over = false;
                    }
                    Err(TakeError::Conflict(m, f)) => link.conflict = Some((m, f)),
                    Err(TakeError::Hid(e)) => return Err(e),
                }
            } else if link.held.is_none()
                && let Some(c) = link.conflict
                && state != c
            {
                link.conflict = None; // the other program let go; claim on the next check
                link.next_check = now;
            }
            if link.conflict.is_none() {
                self.take_over = false; // a take-over answers one conflict, never a later one
            }
        }

        let Some(h) = link.held.as_mut() else { return Ok(()) };
        let refresh_due = link.next_refresh.is_some_and(|t| now >= t);
        if h.base != Some(base) {
            paint(&mut link.kb, &link.statics, base, &frame)?;
            h.base = Some(base);
            h.frame = frame;
        } else if h.frame != frame || refresh_due {
            link.kb.send_frame(&frame)?;
            h.frame = frame;
        } else {
            return Ok(());
        }
        link.next_refresh = refresh.map(|r| now + r);
        Ok(())
    }

    fn status(&self) -> Status {
        let conflict = self.link.as_ref().and_then(|l| l.conflict);
        let control = if let Some((mode, flags)) = conflict {
            Control::Conflict { mode, flags }
        } else if let Some((zone, _)) = self.check {
            Control::Check { zone }
        } else if self.preview.is_some() {
            Control::Preview
        } else if let Some((name, ..)) = &self.active {
            Control::Profile { name: name.clone() }
        } else {
            Control::Onboard
        };
        let held = self.link.as_ref().is_some_and(|l| l.held.is_some());
        let control = match control {
            Control::Onboard | Control::Conflict { .. } => control,
            _ if !held && self.link.is_none() => Control::Waiting,
            _ => control,
        };
        Status {
            device: self.device.clone(),
            control,
            active: self.active.as_ref().map(|a| a.0.clone()),
            message: self.message.clone(),
            pid: std::process::id(),
            hosted: self.hosted,
            controls: self.link.as_ref().map(|l| l.controls.clone()),
            rate_change: self.rate_change.clone(),
        }
    }

    fn shutdown(&mut self) {
        if let Some(link) = &mut self.link
            && let Some(h) = link.held.take()
        {
            match give_back(&mut link.kb, h.restore) {
                Ok(after) => log(&format!("lighting handed back to the keyboard (software control now {:02x} {:02x})", after.0, after.1)),
                Err(e) => log(&format!("could not hand the lighting back ({e}); unplug the keyboard to reset it")),
            }
        }
        log("engine stopped");
    }
}

/// An engine inside the editor process, stopped when the window closes.
pub struct Hosted {
    stop: Arc<AtomicBool>,
    thread: Option<thread::JoinHandle<()>>,
}

impl Hosted {
    /// None when another engine already owns the keyboard.
    pub fn start(opts: Options) -> Result<Option<Hosted>, String> {
        let Some(lock) = lock(false)? else { return Ok(None) };
        let stop = Arc::new(AtomicBool::new(false));
        let flag = stop.clone();
        let thread = thread::spawn(move || {
            if let Err(e) = run(opts, true, flag, lock) {
                log(&e);
            }
        });
        Ok(Some(Hosted { stop, thread: Some(thread) }))
    }
}

impl Drop for Hosted {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}
