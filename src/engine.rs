//! The engine: the one process that opens the keyboard. It keeps the chosen
//! profile applied through replugs and USB re-enumeration, serves the editor and
//! the command line over the local socket, and hands the lighting back to the
//! keyboard when it stops.
//!
//! Ownership of the lighting is recorded twice: on the keyboard (0x8071 software
//! control, mode bit 0) and in $XDG_RUNTIME_DIR/loginux/claim-<device id>, which
//! holds the values to restore. A claim on the keyboard without that file belongs to some
//! other program, and loginux waits for an explicit take-over instead of
//! overwriting it.

use crate::device::{Event, Keyboard, RATES_HZ};
use crate::effects::{self, Effect};
use crate::hidpp::Error;
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
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
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
const RATE_READBACK: Duration = if cfg!(test) { Duration::from_millis(300) } else { Duration::from_secs(5) };
/// After this long without the keyboard, a rate change is reported as failed.
const RATE_GIVE_UP: Duration = Duration::from_secs(20);

pub fn install_signal_handlers() {
    extern "C" fn on_signal(_: libc::c_int) {
        STOP.store(true, Ordering::SeqCst);
    }
    for sig in [libc::SIGTERM, libc::SIGINT, libc::SIGHUP] {
        // SAFETY: the handler only stores to an atomic. Code that waits checks STOP
        // between short sleeps rather than blocking in a system call.
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

/// Ok(None) when another loginux holds the lock, or a signal ended the wait.
/// Waiting retries every 100 ms instead of blocking in flock, so a signal that
/// arrives at any moment ends it.
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
        // SAFETY: flock on an fd we own.
        if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0 {
            let _ = file.set_len(0).and_then(|_| writeln!(file, "{}", std::process::id()));
            return Ok(Some(Lock(file)));
        }
        match io::Error::last_os_error().raw_os_error() {
            Some(libc::EWOULDBLOCK) if wait && !STOP.load(Ordering::SeqCst) => sleep(Duration::from_millis(100)),
            Some(libc::EWOULDBLOCK) => return Ok(None),
            // flock on a descriptor we opened fails otherwise only with EINVAL or ENOLCK
            _ => return Err(format!("{}: {}", path.display(), io::Error::last_os_error())), // LCOV_EXCL_LINE
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
    /// The built-in effect the device is running for us, if any.
    effect: Option<Effect>,
}

#[derive(Debug)]
pub enum TakeError {
    Conflict(u8, u8),
    Hid(Error),
}

impl From<Error> for TakeError {
    fn from(e: Error) -> Self {
        TakeError::Hid(e)
    }
}

/// One claim record per device definition, so two devices never share one.
fn marker(id: &str) -> Option<PathBuf> {
    Some(ipc::runtime_dir().ok()?.join(format!("claim-{id}")))
}

/// (values to restore, software control read back after the claim)
type Marker = ((u8, u8), Option<(u8, u8)>);

fn read_marker(id: &str) -> Option<Marker> {
    let text = fs::read_to_string(marker(id)?).ok()?;
    let pair = |key: &str| -> Option<(u8, u8)> {
        let v = text.lines().find_map(|l| l.strip_prefix(key))?;
        let mut it = v.split_whitespace().map(|b| u8::from_str_radix(b, 16).ok());
        Some((it.next()??, it.next()??))
    };
    Some((pair("restore=")?, pair("held=")))
}

fn write_marker(id: &str, restore: (u8, u8), held: Option<(u8, u8)>) {
    let mut text = format!("restore={:02x} {:02x}\n", restore.0, restore.1);
    if let Some((m, f)) = held {
        text += &format!("held={m:02x} {f:02x}\n");
    }
    if let Some(path) = marker(id) {
        let _ = fs::write(path, text);
    }
}

fn clear_marker(id: &str) {
    if let Some(path) = marker(id) {
        let _ = fs::remove_file(path);
    }
}

/// Takes the lighting effects from the firmware (mode bit 0; the firmware keeps
/// power management). A claim left by an earlier loginux is adopted; anyone
/// else's is a conflict unless `force`, and a forced take-over restores plain
/// firmware control (0, 0) on release.
pub fn take(kb: &mut Keyboard, force: bool) -> Result<Held, TakeError> {
    let id = kb.model.id.clone();
    let now = kb.sw_control()?;
    let restore = if now.0 & 1 == 0 {
        now
    } else {
        match read_marker(&id) {
            Some((restore, held)) if held.is_none_or(|h| h == now) => restore,
            _ if force => (0, 0),
            _ => return Err(TakeError::Conflict(now.0, now.1)),
        }
    };
    write_marker(&id, restore, None);
    kb.set_sw_control(1, 0)?;
    sleep(SETTLE); // let the LED task apply the claim before SetEffect
    let state = kb.sw_control()?;
    write_marker(&id, restore, Some(state));
    Ok(Held { restore, state, base: None, frame: vec![], effect: None })
}

/// Static `base` on every cluster, then the frame twice: the first frame after a
/// claim can be dropped (Solaar replays it too). On a clusters device the frame
/// is one colour per cluster, set once as that cluster's Static effect.
pub fn paint(kb: &mut Keyboard, statics: &[(u8, u8)], base: Rgb, frame: &[(u8, Rgb)]) -> Result<(), Error> {
    if !kb.model.per_key {
        return update(kb, statics, frame);
    }
    for &s in statics {
        kb.set_static(s, base)?;
    }
    sleep(SETTLE);
    kb.send_frame(frame)?;
    kb.send_frame(frame)
}

/// A changed frame, without the claim-time Static reset.
pub fn update(kb: &mut Keyboard, statics: &[(u8, u8)], frame: &[(u8, Rgb)]) -> Result<(), Error> {
    if kb.model.per_key {
        return kb.send_frame(frame);
    }
    for &(cluster, rgb) in frame {
        if let Some(&s) = statics.iter().find(|s| s.0 == cluster) {
            kb.set_static(s, rgb)?;
        }
    }
    Ok(())
}

/// Clusters an effect runs on: every cluster of a per-key device, and the
/// definition's own clusters on a clusters device.
fn targets(model: &Model, clusters: usize) -> impl Iterator<Item = usize> + '_ {
    (0..clusters).filter(move |&c| model.per_key || model.zone(c as u8).is_some())
}

/// Starts `effect` on its clusters, in RAM: as one effect on all of them when
/// it covers every cluster and the device lists it in `multi` (one clock keeps
/// keys and light bar in step), else cluster by cluster. A cluster without
/// that kind of effect shows the effect's colour as Static instead.
pub fn play(kb: &mut Keyboard, lists: &[Vec<u16>], multi: &[u16], statics: &[(u8, u8)], effect: &Effect) -> Result<(), Error> {
    let every = targets(kb.model, lists.len()).count() == lists.len();
    if every && let Ok((id, params)) = effect.encode(multi) {
        let index = multi.iter().position(|&i| i == id).expect("encode picks a listed ID") as u8;
        return kb.set_effect(0xFF, index, params);
    }
    for c in targets(kb.model, lists.len()).collect::<Vec<_>>() {
        let ids = &lists[c];
        match effect.encode(ids) {
            Ok((id, params)) => {
                let index = ids.iter().position(|&i| i == id).expect("encode picks a listed ID") as u8;
                kb.set_effect(c as u8, index, params)?;
            }
            Err(_) => {
                if let Some(&s) = statics.iter().find(|s| usize::from(s.0) == c) {
                    kb.set_static(s, effect.color)?;
                }
            }
        }
    }
    Ok(())
}

/// Effect kinds at least one of the clusters can run, by name; Static is left
/// out because per-key colours cover it.
fn effect_names(model: &Model, lists: &[Vec<u16>]) -> Vec<String> {
    effects::Kind::ALL
        .into_iter()
        .filter(|k| *k != effects::Kind::Static && targets(model, lists.len()).any(|c| k.on(&lists[c])))
        .map(|k| k.name().to_string())
        .collect()
}

/// Zones the device accepts: the 0x8081 bitmap, or the clusters on a clusters device.
pub fn zones(kb: &mut Keyboard, statics: &[(u8, u8)]) -> Result<Vec<u8>, Error> {
    if kb.model.per_key { kb.zones() } else { Ok(statics.iter().map(|s| s.0).collect()) }
}

/// Hands the effects back; the firmware then reloads its onboard effects.
/// Returns what the device reports afterwards.
pub fn give_back(kb: &mut Keyboard, restore: (u8, u8)) -> Result<(u8, u8), Error> {
    kb.set_sw_control(restore.0 & !1, restore.1)?;
    clear_marker(&kb.model.id);
    kb.sw_control()
}

/// For `loginux release` without an engine: the saved values, or plain firmware control.
pub fn release_now(kb: &mut Keyboard) -> Result<(u8, u8), Error> {
    let restore = read_marker(&kb.model.id).map_or((0, 0), |(r, _)| r);
    give_back(kb, restore)
}

// ---- engine ----------------------------------------------------------------

/// What the device should show.
enum Look {
    /// Static base colour and the per-zone frame.
    Colours(Rgb, Vec<(u8, Rgb)>),
    /// A built-in effect the device runs itself.
    Effect(Effect),
}

pub struct Options {
    pub device: Option<PathBuf>,
    /// Resend the frame this often even when nothing changed (off by default).
    pub refresh: Option<Duration>,
}

struct Link {
    kb: Keyboard,
    zones: Vec<u8>,
    statics: Vec<(u8, u8)>,
    /// Effect IDs each cluster lists.
    effects: Vec<Vec<u16>>,
    /// Effect IDs the device can play on all clusters at once.
    multi: Vec<u16>,
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

    let mut engine = Engine::new(opts, hosted);
    log(&format!("engine started (pid {})", std::process::id()));
    drive(&mut engine, &rx, &stop);
    engine.shutdown();
    let _ = fs::remove_file(&sock);
    drop(lock);
    Ok(())
}

/// Answers requests as they come and ticks when none came for a TICK, until
/// `stop`, a signal, or the end of the requests.
fn drive(engine: &mut Engine, rx: &Receiver<Job>, stop: &AtomicBool) {
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
    fn new(opts: Options, hosted: bool) -> Engine {
        let now = Instant::now();
        Engine {
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
        }
    }

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
                let stays = RATES_HZ.get(usize::from(current)).map_or_else(String::new, |r| format!("; it stays at {r} Hz"));
                let error = match e {
                    Error::Device(_, code) => format!("the keyboard refused {hz} Hz (HID++ error {code:#04x}){stays}"),
                    e => e.to_string(),
                };
                log(&format!("report-rate change refused: {error}"));
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
        match profile::load(self.model, &name) {
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
        if let Err(e) = Keyboard::find(self.opts.device.as_deref(), self.model).and_then(|kb| self.link_up(kb)) {
            self.device = match &e {
                Error::NotFound => Device::Missing,
                Error::Access(..) => Device::NoAccess { error: e.to_string() },
                _ => Device::Failed { error: e.to_string() },
            };
            if !matches!(e, Error::NotFound) {
                self.say(&e.to_string());
            }
            let wait = if matches!(e, Error::NotFound) { Duration::from_secs(1) } else { Duration::from_secs(3) };
            self.retry_at = Instant::now() + wait;
        }
    }

    /// Reads what the engine needs from a newly opened device and makes it the link.
    fn link_up(&mut self, mut kb: Keyboard) -> Result<(), Error> {
        let protocol = kb.protocol()?;
        let layout = kb.layout_code()?;
        let statics = kb.static_effects()?;
        let zones = zones(&mut kb, &statics)?;
        let effects = kb.effect_ids()?;
        let multi = optional(kb.multi_cluster_ids().map(Some))?.unwrap_or_default();
        let (controls, rates) = read_controls(&mut kb)?;
        let now = Instant::now();
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
            effects: effect_names(model, &effects),
        };
        self.message = None;
        let current = rates.as_ref().map(|r| r.1);
        self.link = Some(Link {
            kb,
            zones,
            statics,
            effects,
            multi,
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
        Ok(())
    }

    /// What the device should show, or None for its onboard lighting.
    fn desired(&self, zones: &[u8], effects: &[Vec<u16>]) -> Result<Option<Look>, String> {
        if let Some((id, _)) = self.check {
            if !zones.contains(&id) {
                return Err(format!("zone {id} is not reported by this keyboard"));
            }
            // Every key and strip segment dark, the zone under check white.
            let model = self.model;
            let dark = zones.iter().filter(|&&z| model.zone(z).is_some_and(|z| z.kind != Kind::Unknown) && z != id);
            let frame = dark.map(|&z| (z, [0; 3])).chain([(id, [0xFF; 3])]).collect();
            return Ok(Some(Look::Colours([0; 3], frame)));
        }
        let Some(p) = self.preview.as_ref().map(|p| &p.0).or(self.active.as_ref().map(|a| &a.1)) else { return Ok(None) };
        match &p.effect {
            Some(e) if !targets(self.model, effects.len()).any(|c| e.kind.on(&effects[c])) => {
                Err(format!("the {} has no {} effect", self.model.name, e.kind.name()))
            }
            Some(e) => Ok(Some(Look::Effect(e.clone()))),
            None => Ok(Some(Look::Colours(p.base("keys"), p.frame(self.model, zones)?))),
        }
    }

    fn step(&mut self) -> Result<(), Error> {
        let Some((zones, effects)) = self.link.as_ref().map(|l| (l.zones.clone(), l.effects.clone())) else { return Ok(()) };
        let desired = match self.desired(&zones, &effects) {
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

        let Some(look) = desired else {
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
                    clear_marker(&link.kb.model.id);
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
        match look {
            Look::Effect(e) => {
                // The device keeps an effect running; a refresh would only restart it.
                if h.effect.as_ref() == Some(&e) {
                    return Ok(());
                }
                play(&mut link.kb, &link.effects, &link.multi, &link.statics, &e)?;
                h.effect = Some(e);
                (h.base, h.frame) = (None, vec![]); // colours repaint in full after an effect
            }
            Look::Colours(base, frame) => {
                if h.base != Some(base) {
                    paint(&mut link.kb, &link.statics, base, &frame)?;
                    h.base = Some(base);
                    h.frame = frame;
                } else if h.frame != frame || refresh_due {
                    update(&mut link.kb, &link.statics, &frame)?;
                    h.frame = frame;
                } else {
                    return Ok(());
                }
                h.effect = None;
            }
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fake::{self, Fake};

    /// An engine whose configured device does not exist, so it never opens real hardware.
    fn engine() -> Engine {
        Engine::new(Options { device: Some("/nonexistent/loginux-test".into()), refresh: None }, false)
    }

    /// An engine linked to a simulated G316.
    fn linked() -> (Engine, Fake) {
        let mut e = engine();
        let (kb, fake) = fake::keyboard("g316-x-98", fake::g316());
        e.link_up(kb).unwrap();
        (e, fake)
    }

    fn colours(name: &str, esc: Rgb) {
        let m = layout::find("g316-x-98").unwrap();
        let mut p = Profile::default();
        p.bases.insert("keys".into(), [0x10, 0x20, 0x30]);
        p.zones.insert(38, esc);
        profile::save(m, name, &p).unwrap();
    }

    fn ok(reply: Reply) -> Status {
        match reply {
            Reply::Ok(s) => s,
            Reply::Err(e) => panic!("refused: {e}"),
        }
    }

    #[test]
    fn take_records_the_claim_and_give_back_clears_it() {
        let _serial = crate::testenv::serial();
        let (mut kb, fake) = fake::keyboard("g316-x-98", fake::g316());
        clear_marker(&kb.model.id);
        let held = take(&mut kb, false).unwrap();
        assert_eq!((held.restore, held.state, fake.state().sw), ((0, 0), (1, 0), (1, 0)));
        assert_eq!(read_marker(&kb.model.id), Some(((0, 0), Some((1, 0)))));
        assert_eq!(give_back(&mut kb, held.restore).unwrap(), (0, 0));
        assert_eq!((fake.state().sw, read_marker(&kb.model.id)), ((0, 0), None));
    }

    #[test]
    fn another_programs_claim_is_a_conflict_unless_forced() {
        let _serial = crate::testenv::serial();
        let (mut kb, fake) = fake::keyboard("g316-x-98", fake::g316());
        clear_marker(&kb.model.id);
        fake.state().sw = (1, 5);
        assert!(matches!(take(&mut kb, false), Err(TakeError::Conflict(1, 5))));
        assert_eq!(fake.state().sw, (1, 5), "nothing was written");
        let held = take(&mut kb, true).unwrap();
        assert_eq!(held.restore, (0, 0), "a forced take-over hands back plain firmware control");
        release_now(&mut kb).unwrap();
        assert_eq!(fake.state().sw, (0, 0));
    }

    #[test]
    fn an_earlier_loginux_claim_is_adopted() {
        let _serial = crate::testenv::serial();
        let (mut kb, fake) = fake::keyboard("g316-x-98", fake::g316());
        write_marker(&kb.model.id, (0, 6), Some((1, 0)));
        fake.state().sw = (1, 0);
        let held = take(&mut kb, false).unwrap();
        assert_eq!(held.restore, (0, 6), "the original state from before the earlier claim");
        assert_eq!(release_now(&mut kb).unwrap(), (0, 6));
    }

    #[test]
    fn paint_sets_static_then_sends_the_frame_twice() {
        let _serial = crate::testenv::serial();
        let (mut kb, fake) = fake::keyboard("g316-x-98", fake::g316());
        let statics = kb.static_effects().unwrap();
        take(&mut kb, true).unwrap();
        paint(&mut kb, &statics, [1, 2, 3], &[(38, [255, 0, 0]), (39, [0, 255, 0])]).unwrap();
        let st = fake.state();
        assert_eq!(st.effects[&0].0, 1, "cluster 0 runs Static");
        assert_eq!(&st.effects[&1].1[..3], &[1, 2, 3], "with the base colour");
        assert_eq!((st.frames, st.shown[&38], st.shown[&39]), (2, [255, 0, 0], [0, 255, 0]));
    }

    #[test]
    fn a_clusters_device_paints_only_its_defined_lights() {
        let _serial = crate::testenv::serial();
        let (mut kb, fake) = fake::keyboard("g903-hero", fake::g903());
        let statics = kb.static_effects().unwrap();
        assert_eq!(zones(&mut kb, &statics).unwrap(), vec![0, 1]);
        take(&mut kb, true).unwrap();
        paint(&mut kb, &statics, [0; 3], &[(1, [0, 255, 0])]).unwrap();
        let st = fake.state();
        assert_eq!(st.effects.get(&1).map(|e| (e.0, e.1[1])), Some((1, 255)));
        assert!(!st.effects.contains_key(&0), "the DPI indicator is never written");
        assert_eq!(st.frames, 0, "no per-key frames on a mouse");
    }

    #[test]
    fn effects_for_all_clusters_run_as_one() {
        let _serial = crate::testenv::serial();
        let (mut kb, fake) = fake::keyboard("g316-x-98", fake::g316());
        let (statics, lists, multi) = (kb.static_effects().unwrap(), kb.effect_ids().unwrap(), kb.multi_cluster_ids().unwrap());
        assert_eq!(multi, [0x0c, 0x13, 0x16, 0x17, 0x18, 0x19], "as the G316 lists them");
        take(&mut kb, true).unwrap();
        play(&mut kb, &lists, &multi, &statics, &Effect::parse("wave,period=3000").unwrap()).unwrap();
        {
            let st = fake.state();
            assert_eq!((st.effects.len(), st.effects[&0xFF].0), (1, 2), "one wave, on cluster 0xFF: keys and bar on one clock");
        }
        // Cycle is not an all-cluster effect on the G316: it runs on each cluster.
        play(&mut kb, &lists, &multi, &statics, &Effect::parse("cycle").unwrap()).unwrap();
        let st = fake.state();
        assert_eq!((st.effects[&0].0, st.effects[&1].0), (3, 3));
    }

    #[test]
    fn effects_run_on_every_cluster_or_fall_back_to_their_colour() {
        let _serial = crate::testenv::serial();
        let (mut kb, fake) = fake::keyboard("g316-x-98", fake::g316());
        let (statics, lists) = (kb.static_effects().unwrap(), kb.effect_ids().unwrap());
        take(&mut kb, true).unwrap();
        play(&mut kb, &lists, &[], &statics, &Effect::parse("wave,period=3000").unwrap()).unwrap();
        {
            let st = fake.state();
            assert_eq!((st.effects[&0].0, st.effects[&1].0), (4, 4));
        }
        play(&mut kb, &lists, &[], &statics, &Effect::parse("ripple,color=ff0000").unwrap()).unwrap();
        let st = fake.state();
        assert_eq!(st.effects[&0].0, 5, "cluster 0 has ripple (0x17)");
        assert_eq!((st.effects[&1].0, &st.effects[&1].1[..3]), (1, &[255, 0, 0][..]), "cluster 1 shows the colour as Static");
    }

    #[test]
    fn an_effect_for_some_clusters_never_goes_to_all() {
        let _serial = crate::testenv::serial();
        let (mut kb, fake) = fake::keyboard("g903-hero", fake::g903());
        let (statics, lists) = (kb.static_effects().unwrap(), kb.effect_ids().unwrap());
        take(&mut kb, true).unwrap();
        // Even if the mouse listed breathe for all clusters, its DPI cluster stays untouched.
        play(&mut kb, &lists, &[0x0a], &statics, &Effect::parse("breathe").unwrap()).unwrap();
        let st = fake.state();
        assert_eq!(st.effects.keys().copied().collect::<Vec<_>>(), [1], "the logo only");
    }

    #[test]
    fn which_clusters_and_effects_a_device_offers() {
        crate::testenv::init();
        let g316 = layout::find("g316-x-98").unwrap();
        let g903 = layout::find("g903-hero").unwrap();
        let lists = vec![vec![0, 1, 0x0a, 0x15, 0x16, 0x17], vec![0, 1, 0x0a, 0x15, 0x16]];
        assert_eq!(effect_names(g316, &lists), ["breathe", "cycle", "wave", "ripple"]);
        let mouse = vec![vec![0, 1, 3, 0x0a], vec![0, 1, 3, 0x0a]];
        assert_eq!(effect_names(g903, &mouse), ["breathe", "cycle"]);
        assert_eq!(targets(g903, 2).collect::<Vec<_>>(), [1], "only the logo cluster");
        assert_eq!(targets(g316, 2).collect::<Vec<_>>(), [0, 1]);
    }

    #[test]
    fn switching_between_saved_profiles_and_presets() {
        let _serial = crate::testenv::serial();
        let (mut e, fake) = linked();
        clear_marker("g316-x-98");
        colours("T evening", [255, 0, 0]);

        // A saved colour profile: claimed, Static base, the frame drawn.
        let s = ok(e.handle(Request::Use { profile: Some("T evening".into()) }));
        assert_eq!(s.active.as_deref(), Some("T evening"));
        assert_eq!(profile::active().as_deref(), Some("T evening"));
        e.step().unwrap();
        assert_eq!(fake.state().sw, (1, 0));
        {
            let st = fake.state();
            assert_eq!((st.shown[&38], st.shown[&39], st.frames), ([255, 0, 0], [0x10, 0x20, 0x30], 2));
        }
        assert!(matches!(e.status().control, Control::Profile { ref name } if name == "T evening"));

        // A built-in preset: the device runs the wave, no frames are sent.
        ok(e.handle(Request::Use { profile: Some("Wave".into()) }));
        e.step().unwrap();
        assert_eq!(fake.state().effects[&0xFF].0, 2, "the all-cluster wave");
        assert_eq!(&fake.state().effects[&0xFF].1[6..10], &[0x0b, 0xb8, 100, 1], "period 3000 ms, intensity, direction right");
        let frames = fake.state().frames;
        e.step().unwrap();
        assert_eq!(fake.state().frames, frames, "a running effect is not sent again");

        // Back to the colours: Static again on every cluster, then the frame twice.
        ok(e.handle(Request::Use { profile: Some("T evening".into()) }));
        e.step().unwrap();
        {
            let st = fake.state();
            assert_eq!((st.effects[&0].0, st.frames), (1, frames + 2));
        }

        // A hand edit to the file applies on the next reload.
        colours("T evening", [0, 255, 0]);
        e.reload();
        e.step().unwrap();
        assert_eq!(fake.state().shown[&38], [0, 255, 0]);

        // No profile: the lighting goes back to the keyboard.
        ok(e.handle(Request::Use { profile: None }));
        e.step().unwrap();
        assert_eq!(fake.state().sw, (0, 0));
        assert!(matches!(e.status().control, Control::Onboard));
        assert!(matches!(e.handle(Request::Use { profile: Some("T missing".into()) }), Reply::Err(_)));
        profile::delete("T evening").unwrap();
    }

    #[test]
    fn previews_and_channel_checks_lapse_back_to_the_profile() {
        let _serial = crate::testenv::serial();
        let (mut e, fake) = linked();
        colours("T base", [1, 1, 1]);
        ok(e.handle(Request::Use { profile: Some("T base".into()) }));
        e.step().unwrap();

        let mut preview = Profile::default();
        preview.zones.insert(38, [9, 9, 9]);
        ok(e.handle(Request::Preview { profile: Some(preview) }));
        e.step().unwrap();
        assert_eq!(fake.state().shown[&38], [9, 9, 9]);
        assert!(matches!(e.status().control, Control::Preview));

        ok(e.handle(Request::Check { zone: Some(39) }));
        e.step().unwrap();
        {
            let st = fake.state();
            assert_eq!((st.shown[&39], st.shown[&38]), ([0xFF; 3], [0; 3]), "one zone white, the rest dark");
        }
        assert!(matches!(e.status().control, Control::Check { zone: 39 }));
        assert!(matches!(e.handle(Request::Check { zone: Some(250) }), Reply::Err(_)));

        ok(e.handle(Request::Check { zone: None }));
        ok(e.handle(Request::Preview { profile: None }));
        e.step().unwrap();
        assert_eq!(fake.state().shown[&38], [1, 1, 1]);
        let mut stray = Profile::default();
        stray.zones.insert(250, [1, 2, 3]);
        assert!(matches!(e.handle(Request::Preview { profile: Some(stray) }), Reply::Err(_)));
        profile::set_active(None).unwrap();
        profile::delete("T base").unwrap();
    }

    #[test]
    fn a_conflict_waits_for_a_take_over() {
        let _serial = crate::testenv::serial();
        let (mut e, fake) = linked();
        colours("T conflict", [5, 5, 5]);
        ok(e.handle(Request::Use { profile: Some("T conflict".into()) }));
        e.step().unwrap();
        fake.state().sw = (1, 5); // another program claims the lighting
        e.link.as_mut().unwrap().next_check = Instant::now();
        e.step().unwrap();
        assert!(matches!(e.status().control, Control::Conflict { mode: 1, flags: 5 }));
        e.step().unwrap();
        assert_eq!(fake.state().sw, (1, 5), "loginux waits instead of overwriting");
        ok(e.handle(Request::TakeOver));
        e.step().unwrap();
        assert_eq!(fake.state().sw, (1, 0));
        assert!(matches!(e.status().control, Control::Profile { .. }));
        e.shutdown();
        assert_eq!(fake.state().sw, (0, 0), "shutdown hands back plain firmware control");
        profile::set_active(None).unwrap();
        profile::delete("T conflict").unwrap();
    }

    #[test]
    fn report_rate_changes_refusals_and_readback() {
        let _serial = crate::testenv::serial();
        let (mut e, fake) = linked();
        fake.state().refuse_rate = Some((6, 2));
        let Reply::Err(refused) = e.handle(Request::SetReportRate { hz: 8000 }) else { panic!("8000 Hz was accepted") };
        assert_eq!(refused, "the keyboard refused 8000 Hz (HID++ error 0x02); it stays at 1000 Hz");
        assert!(matches!(e.status().rate_change, Some(RateChange::Failed { hz: 8000, .. })));
        assert!(matches!(e.handle(Request::SetReportRate { hz: 1234 }), Reply::Err(_)));
        ok(e.handle(Request::SetReportRate { hz: 1000 }));
        assert!(matches!(e.status().rate_change, Some(RateChange::Done { hz: 1000 })), "already at that rate");
        ok(e.handle(Request::SetReportRate { hz: 2000 }));
        assert_eq!(fake.state().rate, 4);
        assert!(matches!(e.handle(Request::SetReportRate { hz: 4000 }), Reply::Err(_)), "one change at a time");
        e.rate_readback(Some(4));
        assert!(matches!(e.status().rate_change, Some(RateChange::Done { hz: 2000 })));
        assert_eq!(e.status().controls.unwrap().report_rates, [1000, 2000, 4000, 8000]);
    }

    #[test]
    fn effects_a_device_lacks_are_refused_without_a_claim() {
        let _serial = crate::testenv::serial();
        let mut e = engine();
        let (kb, fake) = fake::keyboard("g903-hero", fake::g903());
        e.link_up(kb).unwrap();
        assert!(matches!(&e.status().device, Device::Connected { effects, .. } if effects == &["breathe", "cycle"]));
        ok(e.handle(Request::Use { profile: Some("Wave".into()) }));
        e.step().unwrap();
        assert_eq!(e.status().message.as_deref(), Some("the Logitech G903 HERO has no wave effect"));
        assert_eq!(fake.state().sw, (0, 0), "nothing was claimed");
        ok(e.handle(Request::Use { profile: Some("Breathing".into()) }));
        e.step().unwrap();
        {
            let st = fake.state();
            assert_eq!((st.sw.0, st.effects[&1].0), (1, 3), "breathe is effect 3 on the logo");
        }
        e.shutdown();
    }

    #[test]
    fn no_device_means_no_link_and_a_clear_status() {
        let _serial = crate::testenv::serial();
        let mut e = engine();
        e.connect();
        assert!(e.link.is_none());
        assert!(matches!(e.status().device, Device::NoAccess { .. } | Device::Failed { .. }));
        assert!(matches!(e.handle(Request::SetReportRate { hz: 1000 }), Reply::Err(m) if m.contains("not connected")));
        assert!(matches!(e.handle(Request::Status), Reply::Ok(_)));
        e.tick(); // retries later, never touches other devices
        assert!(e.link.is_none());
    }

    fn preview() -> Option<(Profile, Instant)> {
        Some((Profile::default(), Instant::now() + LEASE))
    }

    fn pending(index: u8, hz: u32, sent: Instant) -> Option<RatePending> {
        Some(RatePending { index, hz, sent })
    }

    fn failed(e: &Engine) -> String {
        match &e.rate_change {
            Some(RateChange::Failed { error, .. }) => error.clone(),
            other => panic!("not a failed change: {other:?}"),
        }
    }

    #[test]
    fn a_failing_keyboard_fails_the_take() {
        let (mut kb, fake) = fake::keyboard("g316-x-98", fake::g316());
        fake.state().refuse = Some(Box::new(|feature, function, _| ((feature, function) == (0x8071, 5)).then_some(4)));
        assert!(matches!(take(&mut kb, false), Err(TakeError::Hid(Error::Device(_, 4)))));
    }

    #[test]
    fn clusters_without_static_are_left_alone() {
        let (mut kb, fake) = fake::keyboard("g903-hero", fake::g903());
        update(&mut kb, &[], &[(1, [1, 2, 3])]).unwrap();
        let lists = kb.effect_ids().unwrap();
        play(&mut kb, &lists, &[], &[], &Effect::parse("ripple").unwrap()).unwrap(); // the logo has no ripple
        assert!(fake.state().effects.is_empty());
    }

    #[test]
    fn optional_reads_fail_only_when_the_keyboard_is_gone() {
        let gone = Error::Gone(io::Error::from_raw_os_error(libc::ENODEV));
        assert!(matches!(optional::<u8>(Err(gone)), Err(Error::Gone(_))));
        let timeout = Error::Timeout(crate::hidpp::Call { feature: 0x8040, function: 0 });
        assert!(matches!(optional::<u8>(Err(timeout)), Ok(None)));
        assert!(matches!(optional(Ok(Some(1))), Ok(Some(1))));
    }

    #[test]
    fn clients_get_a_reply_per_line_until_either_side_leaves() {
        let (tx, rx) = mpsc::channel::<Job>();
        let (ours, theirs) = UnixStream::pair().unwrap();
        let session = thread::spawn(move || client(ours, tx));
        (&theirs).write_all(b"garbage\n").unwrap();
        let mut reply = String::new();
        BufReader::new(&theirs).read_line(&mut reply).unwrap();
        assert!(reply.starts_with("{\"err\":\"bad request"), "{reply}");
        drop(rx); // the engine stopped: the next request ends the session
        let status = serde_json::to_string(&Request::Status).unwrap() + "\n";
        (&theirs).write_all(status.as_bytes()).unwrap();
        session.join().unwrap();

        // A client that leaves before its reply.
        let (tx, _rx) = mpsc::channel::<Job>();
        let (ours, theirs) = UnixStream::pair().unwrap();
        (&theirs).write_all(b"garbage\n").unwrap();
        drop(theirs);
        client(ours, tx);
    }

    #[test]
    fn the_engine_loop_ends_with_its_requests() {
        let _serial = crate::testenv::serial();
        let mut e = engine();
        let (tx, rx) = mpsc::channel::<Job>();
        drop(tx);
        drive(&mut e, &rx, &AtomicBool::new(false));
    }

    #[test]
    fn rate_readbacks_name_what_the_keyboard_reports() {
        let mut e = engine();
        for (current, says) in [
            (Some(3), "the keyboard reports 1000 Hz"),
            (Some(9), "the keyboard reports index 9"),
            (None, "the keyboard did not report its rate after the change"),
        ] {
            e.rate_pending = pending(4, 2000, Instant::now());
            e.rate_readback(current);
            assert_eq!(failed(&e), says);
        }
        e.rate_readback(Some(4));
        assert_eq!(failed(&e), "the keyboard did not report its rate after the change", "nothing was pending");
    }

    #[test]
    fn a_keyboard_without_its_rate_feature_cannot_change_rate() {
        let _serial = crate::testenv::serial();
        let mut e = engine();
        let (kb, _fake) = fake::keyboard("g903-hero", fake::g903());
        e.link_up(kb).unwrap();
        e.link.as_mut().unwrap().rates = Some((vec![3, 4], 3));
        let Reply::Err(error) = e.handle(Request::SetReportRate { hz: 2000 }) else { panic!("accepted") };
        assert_eq!(error, "keyboard does not report HID++ feature 0x8061");
    }

    #[test]
    fn ticks_end_leases_and_give_up_on_a_lost_rate_change() {
        let _serial = crate::testenv::serial();
        let mut e = engine();
        let past = Instant::now() - Duration::from_millis(1);
        e.preview = Some((Profile::default(), past));
        e.check = Some((38, past));
        e.rate_pending = pending(4, 2000, Instant::now() - RATE_GIVE_UP - Duration::from_secs(1));
        e.tick();
        assert!(e.preview.is_none() && e.check.is_none() && e.rate_pending.is_none());
        assert_eq!(failed(&e), "the keyboard did not come back within 20 s; Loginux keeps looking for it");
    }

    #[test]
    fn a_lost_or_failing_keyboard_drops_the_link() {
        let _serial = crate::testenv::serial();
        // Gone after a rate change: logged, not shown, and looked for again soon.
        let (mut e, fake) = linked();
        e.rate_pending = pending(4, 2000, Instant::now());
        fake.unplug();
        e.tick();
        assert!(e.link.is_none() && matches!(e.device, Device::Missing) && e.message.is_none());
        // A keyboard that rejects what it should answer: shown, and retried later.
        let (mut e, fake) = linked();
        e.preview = preview();
        fake.state().refuse = Some(Box::new(|feature, function, _| ((feature, function) == (0x8071, 5)).then_some(4)));
        let before = Instant::now();
        e.tick();
        assert!(e.link.is_none() && matches!(e.device, Device::Failed { .. }));
        assert!(e.message.as_deref().unwrap().contains("HID++ error 0x04"));
        assert!(e.retry_at >= before + Duration::from_secs(5));
    }

    #[test]
    fn reload_follows_config_and_keeps_a_broken_profiles_last_version() {
        let _serial = crate::testenv::serial();
        let mut e = engine();
        colours("T reload", [1, 2, 3]);
        profile::set_active(Some("T reload")).unwrap();
        e.reload();
        let stamp = e.active.as_ref().unwrap().2;
        e.reload(); // unchanged
        assert_eq!(e.active.as_ref().unwrap().2, stamp);
        let path = profile::path("T reload").unwrap();
        fs::write(&path, "zones = ").unwrap();
        let later = SystemTime::now() + Duration::from_secs(5);
        File::options().write(true).open(&path).unwrap().set_modified(later).unwrap();
        e.reload();
        let (name, kept, stamp) = e.active.as_ref().unwrap();
        assert_eq!((name.as_str(), kept.zones[&38], *stamp), ("T reload", [1, 2, 3], Some(later)));
        assert!(e.message.as_deref().unwrap().contains("T reload.toml"));

        let config = crate::testenv::init().join("config/loginux/config.toml");
        fs::write(&config, "profile = \"../elsewhere\"\n").unwrap();
        e.reload();
        assert!(e.active.is_none());
        assert_eq!(e.message.as_deref(), Some("config.toml names an invalid profile \"../elsewhere\""));
        profile::set_active(Some("T reload")).unwrap();
        e.reload();
        profile::set_active(Some("T absent")).unwrap(); // a profile with no file: nothing to keep
        e.reload();
        assert!(e.active.is_none());
        profile::set_active(None).unwrap();
        profile::delete("T reload").unwrap();
    }

    #[test]
    fn connecting_finds_plugged_keyboards_and_explains_failures() {
        let _serial = crate::testenv::serial();
        let mut e = Engine::new(Options { device: None, refresh: None }, false);
        e.connect();
        assert!(matches!(e.device, Device::Missing) && e.message.is_none(), "nothing plugged in: no message");
        // A G903 beside an unplugged G316 is not taken for it (the replug of 2026-10-22).
        let mouse = crate::testenv::init().join("sysfs/class/hidraw/hidraw-fake3");
        crate::testenv::hid_node(&mouse, "0003:0000046D:0000C091", "02");
        e.connect();
        fs::remove_dir_all(&mouse).unwrap();
        assert!(matches!(e.device, Device::Missing) && e.model.id == "g316-x-98");
        let file = crate::testenv::init().join("not-a-keyboard");
        fs::write(&file, "").unwrap();
        let mut e = Engine::new(Options { device: Some(file), refresh: None }, false);
        e.connect();
        assert!(matches!(&e.device, Device::Failed { error } if error.contains("not the HID++ interface")));
        let _fake = fake::plug("/dev/fake-connect", "g316-x-98", fake::g316());
        let mut e = Engine::new(Options { device: Some("/dev/fake-connect".into()), refresh: None }, false);
        e.connect();
        assert!(e.link.is_some() && matches!(&e.device, Device::Connected { path, .. } if path == "/dev/fake-connect"));
    }

    #[test]
    fn a_keyboard_back_from_a_rate_change_is_read_back() {
        let _serial = crate::testenv::serial();
        let mut spec = fake::g316();
        spec.zones.retain(|&z| z != 38); // this one does not report Esc
        let (kb, _fake) = fake::keyboard("g316-x-98", spec);
        let mut e = engine();
        e.rate_pending = pending(3, 1000, Instant::now());
        e.link_up(kb).unwrap();
        assert!(matches!(e.rate_change, Some(RateChange::Done { hz: 1000 })));
        e.check = Some((38, Instant::now() + LEASE));
        let link = e.link.as_ref().unwrap();
        assert_eq!(e.desired(&link.zones, &link.effects).err().as_deref(), Some("zone 38 is not reported by this keyboard"));
    }

    #[test]
    fn controls_follow_notifications_and_periodic_reads() {
        let _serial = crate::testenv::serial();
        let (mut e, fake) = linked();
        fake.notify(13, 0, &[1]); // 0x4523 game mode, at feature index 13 on the G316
        e.step().unwrap();
        assert_eq!(e.link.as_ref().unwrap().controls.game_mode, Some(true));
        e.link.as_mut().unwrap().next_controls = Instant::now();
        e.step().unwrap();
        assert_eq!(e.link.as_ref().unwrap().controls.game_mode, Some(false), "read again from the keyboard");
        // A rate change the keyboard kept its connection through: read back on schedule.
        fake.state().rate = 4;
        e.rate_pending = pending(4, 2000, Instant::now() - RATE_READBACK - Duration::from_secs(1));
        e.step().unwrap();
        assert!(matches!(e.rate_change, Some(RateChange::Done { hz: 2000 })));
        assert_eq!(e.link.as_ref().unwrap().controls.report_rate, Some(2000));
    }

    #[test]
    fn claims_lost_refused_and_released_by_others() {
        let _serial = crate::testenv::serial();
        clear_marker("g316-x-98");
        let (mut e, fake) = linked();
        e.preview = preview();
        // Another program holds the lighting before loginux claims it.
        fake.state().sw = (1, 5);
        e.step().unwrap();
        assert_eq!(e.link.as_ref().unwrap().conflict, Some((1, 5)));
        // It lets go: the conflict ends, and the next check claims.
        fake.state().sw = (0, 0);
        e.link.as_mut().unwrap().next_check = Instant::now();
        e.step().unwrap();
        assert_eq!(e.link.as_ref().unwrap().conflict, None);
        e.step().unwrap();
        assert_eq!(fake.state().sw, (1, 0));
        // The keyboard takes its lighting back (a reset): claimed again.
        fake.state().sw = (0, 0);
        e.link.as_mut().unwrap().next_check = Instant::now();
        e.step().unwrap();
        assert_eq!(fake.state().sw, (1, 0));
        // A claim the keyboard refuses ends the link.
        fake.state().sw = (0, 0);
        fake.state().refuse = Some(Box::new(|feature, function, p| (feature == 0x8071 && function == 5 && p[0] == 1).then_some(4)));
        e.link.as_mut().unwrap().next_check = Instant::now();
        assert!(matches!(e.step(), Err(Error::Device(_, 4))));
        clear_marker("g316-x-98");
    }

    #[test]
    fn shutdown_reports_a_keyboard_it_cannot_hand_back_to() {
        let _serial = crate::testenv::serial();
        let (mut e, fake) = linked();
        e.preview = preview();
        e.step().unwrap();
        assert!(e.link.as_ref().unwrap().held.is_some());
        fake.unplug();
        e.shutdown(); // logs, and leaves the claim recorded for the next engine
        assert!(read_marker("g316-x-98").is_some());
        clear_marker("g316-x-98");
    }

    #[test]
    fn a_hosted_engine_logs_a_socket_it_cannot_bind() {
        let _serial = crate::testenv::serial();
        let sock = ipc::socket_path().unwrap();
        let _ = fs::remove_file(&sock);
        fs::create_dir(&sock).unwrap(); // something else is in the way
        let hosted = Hosted::start(Options { device: Some("/nonexistent/loginux-test".into()), refresh: None });
        drop(hosted.unwrap().expect("the lock was free"));
        fs::remove_dir(&sock).unwrap();
        assert!(lock(false).unwrap().is_some(), "the failed engine let go of the lock");
    }

    #[test]
    fn waiting_for_the_lock_ends_with_it_or_with_a_signal() {
        let _serial = crate::testenv::serial();
        let held = lock(false).unwrap().expect("free");
        let waiter = thread::spawn(|| lock(true));
        thread::sleep(Duration::from_millis(250));
        assert!(!waiter.is_finished(), "still waiting");
        drop(held);
        assert!(waiter.join().unwrap().unwrap().is_some(), "the lock, once it was free");
        let held = lock(false).unwrap().expect("free");
        let waiter = thread::spawn(|| lock(true));
        thread::sleep(Duration::from_millis(150));
        STOP.store(true, Ordering::SeqCst); // what the signal handler does
        let result = waiter.join().unwrap();
        STOP.store(false, Ordering::SeqCst);
        drop(held);
        assert!(result.unwrap().is_none());
    }

    #[test]
    fn only_one_engine_holds_the_lock() {
        let _serial = crate::testenv::serial();
        let first = lock(false).unwrap().expect("free");
        assert!(lock(false).unwrap().is_none());
        assert_eq!(owner(), Some(std::process::id()));
        drop(first);
        assert!(lock(false).unwrap().is_some());
    }
}
