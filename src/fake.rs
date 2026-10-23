//! A simulated HID++ device for tests: the far end of a socket pair, answering
//! the requests Loginux sends the way a G316 X 98 or a G903 on its cable does
//! (layouts from their live probes) and recording every request. No hardware
//! is opened.
//!
//! It answers the way the real devices answered once, and nothing more: a
//! passing test cannot show what a device does with a request it was never
//! seen to handle. A change to what Loginux sends needs a check on the device
//! as well (CONTRIBUTING.md, Tests).

use crate::device::Keyboard;
use crate::hidpp::Hidpp;
use crate::layout::{self, Model};
use crate::profile::Rgb;
use std::collections::BTreeMap;
use std::fs::File;
use std::io::{Read, Write};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, TryLockError};
use std::thread;
use std::time::{Duration, Instant};

/// What the simulated device reports.
#[derive(Clone)]
pub struct Spec {
    /// Feature IDs by index; index 0 is IRoot.
    pub features: Vec<u16>,
    /// Per 0x8071 cluster: location, display-persistency byte, effect IDs by index.
    pub clusters: Vec<(u16, u8, Vec<u16>)>,
    /// 0x8071 non-volatile capabilities: capability, then state and six parameters.
    pub nv: Vec<(u16, [u8; 7])>,
    pub ext_caps: u16,
    /// 0x8071 effect IDs playable on all clusters at once (cluster 0xFF).
    pub multi: Vec<u16>,
    /// 0x8061/3 is refused (HID++ error 2) until 0x8101/6 [05] puts profile
    /// management in host mode, as on the G316.
    pub rate_needs_host: bool,
    /// 0x8081 zones.
    pub zones: Vec<u8>,
}

/// Decides, per request, whether the device rejects it: from the feature ID,
/// function and parameters, Some(HID++ error) rejects.
pub type Refuse = Box<dyn FnMut(u16, u8, &[u8]) -> Option<u8> + Send>;

/// What the simulated device was told, and its settings.
#[derive(Default)]
pub struct State {
    /// 0x8071 software control: mode and event flags.
    pub sw: (u8, u8),
    /// Per cluster: effect index, parameters and persistence byte of the last SetEffect.
    pub effects: BTreeMap<u8, (u8, [u8; 10], u8)>,
    pub staged: BTreeMap<u8, Rgb>,
    /// Colours drawn by FrameEnd, accumulated.
    pub shown: BTreeMap<u8, Rgb>,
    pub frames: usize,
    /// The 0x8040 brightness it reports; a test changes it as the knob would.
    pub brightness: u16,
    pub rate: u8,
    /// A rate index the device refuses, and the HID++ error it answers with.
    pub refuse_rate: Option<(u8, u8)>,
    pub game_mode: bool,
    /// Profile management is in host mode (0x8101/6 [05]); [03] ends it.
    pub profiles_host: bool,
    pub refuse: Option<Refuse>,
    /// Edits each reply payload before it is sent: feature ID, function, payload.
    pub mangle: Option<fn(u16, u8, &mut Vec<u8>)>,
    /// Every request: feature ID, function and the 16 parameter bytes.
    pub log: Vec<(u16, u8, Vec<u8>)>,
}

pub struct Fake {
    state: Arc<Mutex<State>>,
    /// A second handle on the device end, for notifications and unplugging.
    io: File,
    device: u8,
}

impl Fake {
    /// The device's state. Waiting more than two seconds means this thread
    /// already holds it (two `fake.state()` in one statement), which panics
    /// instead of hanging the test run.
    pub fn state(&self) -> MutexGuard<'_, State> {
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            match self.state.try_lock() {
                Ok(guard) => return guard,
                Err(TryLockError::Poisoned(p)) => return p.into_inner(),
                Err(TryLockError::WouldBlock) if Instant::now() < deadline => thread::sleep(Duration::from_millis(1)),
                Err(TryLockError::WouldBlock) => panic!("the fake's state is still locked: two fake.state() in one statement?"),
            }
        }
    }

    /// Requests for one feature, in order: function and parameters.
    pub fn requests(&self, feature: u16) -> Vec<(u8, Vec<u8>)> {
        self.state().log.iter().filter(|r| r.0 == feature).map(|r| (r.1, r.2.clone())).collect()
    }

    /// An unsolicited report (software ID 0), as the device sends on its own.
    pub fn notify(&self, index: u8, function: u8, params: &[u8]) {
        let mut out = [0u8; 20];
        out[..4].copy_from_slice(&[0x11, self.device, index, function << 4]);
        out[4..4 + params.len()].copy_from_slice(params);
        (&self.io).write_all(&out).expect("notification");
    }

    /// Pulls the plug: the next read on Loginux's side finds the device gone.
    pub fn unplug(&self) {
        // SAFETY: shutting down a socket this struct owns a handle to.
        unsafe { libc::shutdown(self.io.as_raw_fd(), libc::SHUT_RDWR) };
    }
}

pub fn g316() -> Spec {
    let model = layout::find("g316-x-98").expect("built-in G316");
    let mut zones: Vec<u8> = model.zones.iter().map(|z| z.id).collect();
    zones.sort_unstable();
    Spec {
        features: vec![
            0x0000, 0x0001, 0x0003, 0x0005, 0x0020, 0x1e00, 0x8061, 0x8071, 0x8081, 0x00c3, 0x00d0, 0x18b0, 0x1b10,
            0x4523, 0x4540, 0x8040, 0x8101,
        ],
        clusters: vec![
            (0x0001, 0, vec![0x0000, 0x0001, 0x000a, 0x0015, 0x0016, 0x0017]),
            (0x0005, 0, vec![0x0000, 0x0001, 0x000a, 0x0015, 0x0016]),
        ],
        nv: vec![
            (0x0001, [1, 0, 0, 0, 0, 0, 0]),
            (0x0002, [2, 0, 0, 0, 0, 0, 0]),
            (0x0020, [1, 30, 0, 0, 0, 0, 0]),
            (0x0040, [1, 0, 0, 0, 0, 0, 0]),
        ],
        ext_caps: 0x0026,
        multi: vec![0x000c, 0x0013, 0x0016, 0x0017, 0x0018, 0x0019],
        rate_needs_host: true,
        zones,
    }
}

pub fn g903() -> Spec {
    Spec {
        features: vec![0x0000, 0x0001, 0x0003, 0x0005, 0x1d4b, 0x0020, 0x1001, 0x8071, 0x8100, 0x8110, 0x8060, 0x2201],
        clusters: vec![
            (0x0001, 0x07, vec![0x0000, 0x0001, 0x0003, 0x000a]),
            (0x0002, 0x00, vec![0x0000, 0x0001, 0x0003, 0x000a]),
        ],
        nv: vec![
            (0x0001, [1, 0xff, 0x03, 0, 0, 0, 0]),
            (0x0002, [2, 0xff, 0x03, 0, 0, 0, 0]),
            (0x0008, [1, 0x07, 0, 0, 0, 0, 0]),
        ],
        ext_caps: 0x0004,
        multi: vec![],
        rate_needs_host: false,
        zones: vec![],
    }
}

/// A transport talking to a simulated device.
pub fn transport(spec: Spec, device: u8) -> (Hidpp, Fake) {
    crate::testenv::init();
    let mut fds = [0; 2];
    // SAFETY: `fds` has room for the two descriptors socketpair writes.
    let r = unsafe { libc::socketpair(libc::AF_UNIX, libc::SOCK_SEQPACKET | libc::SOCK_CLOEXEC, 0, fds.as_mut_ptr()) };
    assert_eq!(r, 0, "socketpair");
    // SAFETY: both descriptors were just created and nothing else owns them.
    let (ours, theirs) = unsafe { (File::from(OwnedFd::from_raw_fd(fds[0])), File::from(OwnedFd::from_raw_fd(fds[1]))) };
    let state = Arc::new(Mutex::new(State { rate: 3, brightness: 100, ..State::default() }));
    let fake = Fake { state: state.clone(), io: theirs.try_clone().expect("second handle"), device };
    thread::spawn(move || serve(theirs, spec, state));
    (Hidpp::from_file(ours, device), fake)
}

/// A transport whose device never answers; the returned end keeps it open.
pub fn silent(device: u8) -> (Hidpp, File) {
    let mut fds = [0; 2];
    // SAFETY: `fds` has room for the two descriptors socketpair writes.
    let r = unsafe { libc::socketpair(libc::AF_UNIX, libc::SOCK_SEQPACKET | libc::SOCK_CLOEXEC, 0, fds.as_mut_ptr()) };
    assert_eq!(r, 0, "socketpair");
    // SAFETY: both descriptors were just created and nothing else owns them.
    let (ours, theirs) = unsafe { (File::from(OwnedFd::from_raw_fd(fds[0])), File::from(OwnedFd::from_raw_fd(fds[1]))) };
    (Hidpp::from_file(ours, device), theirs)
}

static PLUGGED: Mutex<Vec<(PathBuf, Hidpp, &'static Model)>> = Mutex::new(Vec::new());

/// Plugs a simulated device in at `path`, as the built-in definition `id`: the
/// next `Keyboard::find` or `device::probe` of that path opens it. Tests pick
/// paths that no real device has.
pub fn plug(path: &str, id: &str, spec: Spec) -> Fake {
    let model = layout::find(id).expect("built-in definition");
    let (io, fake) = transport(spec, model.device_index);
    PLUGGED.lock().unwrap_or_else(|p| p.into_inner()).push((path.into(), io, model));
    fake
}

/// The device plugged in at `path`, taken out of the list.
pub fn plugged(path: &Path) -> Option<(Hidpp, &'static Model)> {
    let mut all = PLUGGED.lock().unwrap_or_else(|p| p.into_inner());
    let i = all.iter().position(|(p, ..)| p == path)?;
    let (_, io, model) = all.remove(i);
    Some((io, model))
}

/// A Keyboard for the built-in definition `id`, on a simulated device.
pub fn keyboard(id: &str, spec: Spec) -> (Keyboard, Fake) {
    crate::testenv::init();
    let model = layout::find(id).expect("built-in definition");
    let (io, fake) = transport(spec, model.device_index);
    let kb = Keyboard::attach(io, Path::new("/dev/fake"), model).expect("the simulated device answers");
    (kb, fake)
}

fn serve(mut io: File, spec: Spec, state: Arc<Mutex<State>>) {
    let mut buf = [0u8; 64];
    while let Ok(n) = io.read(&mut buf) {
        if n < 4 {
            return; // closed
        }
        let (dev, index, fsw) = (buf[1], buf[2], buf[3]);
        let mut params = [0u8; 16];
        params[..n.min(20) - 4].copy_from_slice(&buf[4..n.min(20)]);
        let result = {
            let mut st = state.lock().unwrap_or_else(|p| p.into_inner());
            match spec.features.get(usize::from(index)) {
                Some(&feature) => {
                    let function = fsw >> 4;
                    st.log.push((feature, function, params.to_vec()));
                    match st.refuse.as_mut().and_then(|refuse| refuse(feature, function, &params)) {
                        Some(code) => Err(code),
                        None => {
                            let mut reply = answer(&spec, &mut st, feature, function, &params);
                            if let (Ok(payload), Some(mangle)) = (&mut reply, st.mangle) {
                                mangle(feature, function, payload);
                            }
                            reply
                        }
                    }
                }
                None => Err(6), // invalid feature index
            }
        };
        let mut out = [0u8; 20];
        match result {
            Ok(payload) => {
                out[..4].copy_from_slice(&[0x11, dev, index, fsw]);
                out[4..4 + payload.len()].copy_from_slice(&payload);
            }
            Err(code) => out[..6].copy_from_slice(&[0x11, dev, 0xFF, index, fsw, code]),
        }
        if io.write_all(&out).is_err() {
            return;
        }
    }
}

/// The reply payload, or a HID++ error code.
fn answer(spec: &Spec, st: &mut State, feature: u16, function: u8, p: &[u8]) -> Result<Vec<u8>, u8> {
    match (feature, function) {
        (0x0000, 0) => {
            let id = u16::from_be_bytes([p[0], p[1]]);
            Ok(vec![spec.features.iter().position(|&f| f == id).unwrap_or(0) as u8, 0, 0])
        }
        (0x0000, 1) => Ok(vec![4, 2, p[2]]),
        (0x0001, 0) => Ok(vec![(spec.features.len() - 1) as u8]),
        (0x0001, 1) => {
            let [hi, lo] = spec.features.get(usize::from(p[0])).ok_or(2u8)?.to_be_bytes();
            Ok(vec![hi, lo, 0, 0])
        }
        (0x8071, 0) => {
            let (cluster, effect) = (p[0], p[1]);
            if (cluster, effect) == (0xFF, 0xFF) {
                let [nh, nl] = spec.nv.iter().fold(0u16, |a, n| a | n.0).to_be_bytes();
                let [eh, el] = spec.ext_caps.to_be_bytes();
                return Ok(vec![0xFF, 0, spec.clusters.len() as u8, nh, nl, eh, el, spec.multi.len() as u8]);
            }
            if cluster == 0xFF {
                let [ih, il] = spec.multi.get(usize::from(effect)).ok_or(2u8)?.to_be_bytes();
                return Ok(vec![0xFF, effect, ih, il, 0, 0, 0, 0]);
            }
            let (location, persistency, ids) = spec.clusters.get(usize::from(cluster)).ok_or(2u8)?;
            if effect == 0xFF {
                let [lh, ll] = location.to_be_bytes();
                return Ok(vec![cluster, 0, lh, ll, ids.len() as u8, *persistency, 0, 0]);
            }
            let [ih, il] = ids.get(usize::from(effect)).ok_or(2u8)?.to_be_bytes();
            Ok(vec![cluster, effect, ih, il, 0, 0, 0, 0])
        }
        (0x8071, 1) => {
            if st.sw.0 & 1 == 0 {
                return Err(5); // not allowed without software control of the clusters
            }
            let ids = if p[0] == 0xFF { &spec.multi } else { &spec.clusters.get(usize::from(p[0])).ok_or(2u8)?.2 };
            if usize::from(p[1]) >= ids.len() {
                return Err(2);
            }
            let mut params = [0u8; 10];
            params.copy_from_slice(&p[2..12]);
            st.effects.insert(p[0], (p[1], params, p[12]));
            Ok(vec![p[0], p[1]])
        }
        (0x8071, 3) if p[0] == 0 => {
            let cap = u16::from_be_bytes([p[1], p[2]]);
            let (_, value) = spec.nv.iter().find(|n| n.0 == cap).ok_or(2u8)?;
            let mut out = vec![0, p[1], p[2]];
            out.extend_from_slice(value);
            Ok(out)
        }
        (0x8071, 5) => {
            if p[0] == 1 {
                st.sw = (p[1], p[2]);
            }
            Ok(vec![p[0], st.sw.0, st.sw.1])
        }
        (0x8081, 0) => {
            let page = usize::from(p[1]);
            let mut bits = [0u8; 14];
            for z in spec.zones.iter().map(|&z| usize::from(z)).filter(|z| z / 112 == page) {
                bits[(z % 112) / 8] |= 1 << (z % 8);
            }
            let mut out = vec![0, p[1]];
            out.extend_from_slice(&bits);
            Ok(out)
        }
        (0x8081, 1) => {
            for rec in p.chunks(4).filter(|r| r[0] != 0xFF) {
                st.staged.insert(rec[0], [rec[1], rec[2], rec[3]]);
            }
            Ok(p.chunks(4).map(|r| r[0]).collect())
        }
        (0x8081, 7) => {
            let staged = std::mem::take(&mut st.staged);
            st.shown.extend(staged);
            st.frames += 1;
            Ok(vec![])
        }
        // 0x8070 on older devices: one zone at location 1 with two effects, IDs 1 and 2.
        (0x8070, 0) => Ok(vec![1]),
        (0x8070, 1) => Ok(vec![p[0], 0, 1, 2]),
        (0x8070, 2) => Ok(vec![p[0], p[1], 0, p[1] + 1]),
        (0x4540, 0) => Ok(vec![0x01]),
        (0x8040, 0) => Ok(vec![0, 100, 5, 3]),
        (0x8040, 1) => Ok(st.brightness.to_be_bytes().to_vec()),
        (0x8061, 1) => Ok(vec![0, 0x78]),
        (0x8061, 2) => Ok(vec![st.rate]),
        (0x8061, 3) => match st.refuse_rate {
            Some((index, code)) if index == p[0] => Err(code),
            _ if spec.rate_needs_host && !st.profiles_host => Err(2),
            _ => {
                st.rate = p[0];
                Ok(vec![])
            }
        },
        (0x4523, 0) => Ok(vec![u8::from(st.game_mode)]),
        (0x8101, 6) => {
            st.profiles_host = p[0] == 0x05;
            Ok(vec![])
        }
        (0x8100, 0) => Ok(vec![1, 4, 1, 5, 1, 0x0c, 0x10, 0, 0xff, 0x0a, 4, 0, 0, 0, 0, 0]),
        (0x8100, 2) => Ok(vec![1, 0, 0, 0]),
        _ => Err(7), // invalid function
    }
}
