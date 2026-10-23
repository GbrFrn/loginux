//! HID++ 2.0 transport over a keyboard's hidraw node.
//!
//! Only the HID++ interface named by a keyboard definition (USB vendor, product
//! and interface number) is opened, and the check is repeated on the opened file
//! descriptor, so a renumbered /dev/hidrawN can never point this at another
//! device. `open_unchecked` exists for `loginux probe` alone, which sends nothing
//! but reads.

use crate::layout::Model;
use std::collections::VecDeque;
use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::fs::{FileTypeExt, MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

const LONG: u8 = 0x11;
const REPLY_TIMEOUT: Duration = Duration::from_secs(1);

/// Which request an error belongs to, by feature ID rather than index.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Call {
    pub feature: u16,
    pub function: u8,
}

#[derive(Debug)]
pub enum Error {
    NotFound,
    Access(PathBuf, io::Error),
    WrongDevice(PathBuf),
    Gone(io::Error),
    Timeout(Call),
    Device(Call, u8),
    Busy(Call),
    Unsupported(u16),
    Reply(Call, String),
    Refused(Call),
}

pub const BUSY: u8 = 0x08;

impl fmt::Display for Call {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "feature {:#06x} function {}", self.feature, self.function)
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            Error::NotFound => write!(f, "no supported keyboard found (`loginux zones` names the definitions)"),
            Error::Access(p, e) => write!(
                f,
                "cannot open {}: {e}; install the udev rule (see README) and replug the keyboard",
                p.display()
            ),
            Error::WrongDevice(p) => write!(f, "{} is not the HID++ interface of a known keyboard", p.display()),
            Error::Gone(e) => write!(f, "keyboard disconnected ({e})"),
            Error::Timeout(c) => write!(f, "no reply to {c}"),
            Error::Device(c, code) => write!(f, "keyboard rejected {c} with HID++ error {code:#04x}"),
            Error::Busy(c) => write!(f, "keyboard stayed busy on {c} (is the backlight switched off?)"),
            Error::Unsupported(id) => write!(f, "keyboard does not report HID++ feature {id:#06x}"),
            Error::Reply(c, why) => write!(f, "unexpected reply to {c}: {why}"),
            Error::Refused(c) => write!(f, "refusing to send {c}: outside loginux's write boundary"),
        }
    }
}

/// USB identity of a hidraw class directory (/sys/class/hidraw/hidrawN or
/// /sys/dev/char/M:m): (vendor, product, interface number, HID name).
pub fn identity(class_dir: &Path) -> Option<(u16, u16, u8, String)> {
    let device = class_dir.join("device");
    let uevent = fs::read_to_string(device.join("uevent")).ok()?;
    let field = |key: &str| uevent.lines().find_map(|l| l.strip_prefix(key)).map(str::to_string);
    let id = field("HID_ID=")?; // bus:vendor:product, hex
    let mut parts = id.split(':').map(|p| u32::from_str_radix(p, 16).ok());
    let (bus, vendor, product) = (parts.next()??, parts.next()??, parts.next()??);
    if bus != 0x0003 {
        return None; // USB only
    }
    let usb_interface = fs::canonicalize(&device).ok()?.parent()?.join("bInterfaceNumber");
    let interface = u8::from_str_radix(fs::read_to_string(usb_interface).ok()?.trim(), 16).ok()?;
    Some((vendor as u16, product as u16, interface, field("HID_NAME=").unwrap_or_default()))
}

fn is_model(class_dir: &Path, m: &Model) -> bool {
    identity(class_dir).is_some_and(|(v, p, i, _)| (v, p, i) == (m.vendor, m.product, m.interface))
}

static SYSFS: OnceLock<PathBuf> = OnceLock::new();

/// Where sysfs is mounted. Tests get a scratch tree instead (`use_sysfs`), so
/// no test can find, or open, a device plugged into the machine running it.
fn sysfs() -> &'static Path {
    #[cfg(test)]
    crate::testenv::init();
    SYSFS.get_or_init(|| PathBuf::from("/sys"))
}

#[cfg(test)]
pub fn use_sysfs(root: PathBuf) {
    SYSFS.set(root).expect("the sysfs root is chosen before any lookup");
}

fn nodes() -> Vec<PathBuf> {
    let mut nodes: Vec<PathBuf> =
        fs::read_dir(sysfs().join("class/hidraw")).into_iter().flatten().filter_map(|e| Some(e.ok()?.path())).collect();
    nodes.sort();
    nodes
}

/// The first hidraw node that is the HID++ interface of one of `models`.
pub fn find(models: &'static [Model]) -> Result<(PathBuf, &'static Model), Error> {
    for node in nodes() {
        if let Some(m) = models.iter().find(|m| is_model(&node, m)) {
            return Ok((Path::new("/dev").join(node.file_name().ok_or(Error::NotFound)?), m));
        }
    }
    Err(Error::NotFound)
}

/// Is the model's HID++ interface plugged in?
pub fn present(m: &Model) -> bool {
    nodes().iter().any(|n| is_model(n, m))
}

/// sysfs class directory of an opened character device, from its device number.
fn class_dir(file: &File, path: &Path) -> Result<PathBuf, Error> {
    let meta = file.metadata().map_err(|e| Error::Access(path.into(), e))?;
    if !meta.file_type().is_char_device() {
        return Err(Error::WrongDevice(path.into()));
    }
    let rdev = meta.rdev();
    // glibc's dev_t encoding, as in gnu_dev_major/gnu_dev_minor.
    let major = ((rdev >> 32) & 0xffff_f000) | ((rdev >> 8) & 0x0fff);
    let minor = ((rdev >> 12) & 0xffff_ff00) | (rdev & 0x00ff);
    Ok(sysfs().join(format!("dev/char/{major}:{minor}")))
}

/// The model whose HID++ interface `path` is, checked through sysfs.
pub fn identify(path: &Path, models: &'static [Model]) -> Result<&'static Model, Error> {
    let file = File::open(path).map_err(|e| Error::Access(path.into(), e))?;
    let dir = class_dir(&file, path)?;
    models.iter().find(|m| is_model(&dir, m)).ok_or_else(|| Error::WrongDevice(path.into()))
}

pub struct Hidpp {
    file: File,
    device: u8,
    swid: u8,
    /// Unsolicited reports (software ID 0) seen while waiting for replies.
    notes: VecDeque<[u8; 20]>,
}

impl Hidpp {
    /// Opens `path` only if it is the HID++ interface of `model`.
    pub fn open(path: &Path, model: &Model) -> Result<Self, Error> {
        let io = Self::open_unchecked(path, model.device_index)?;
        if !is_model(&class_dir(&io.file, path)?, model) {
            return Err(Error::WrongDevice(path.into()));
        }
        Ok(io)
    }

    /// Any hidraw node, for `loginux probe`'s read-only queries.
    pub fn open_unchecked(path: &Path, device: u8) -> Result<Self, Error> {
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .custom_flags(libc::O_NONBLOCK)
            .open(path)
            .map_err(|e| Error::Access(path.into(), e))?;
        class_dir(&file, path)?; // a character device, or nothing is sent
        Ok(Hidpp { file, device, swid: 0, notes: VecDeque::new() })
    }

    /// A transport over an already open file, such as one end of a socket pair
    /// in tests; production code goes through `open`, which checks the device.
    #[cfg(test)]
    pub fn from_file(file: File, device: u8) -> Self {
        Hidpp { file, device, swid: 0, notes: VecDeque::new() }
    }

    /// sysfs identity of the opened node.
    pub fn identity(&self, path: &Path) -> Option<(u16, u16, u8, String)> {
        identity(&class_dir(&self.file, path).ok()?)
    }

    /// One request and its reply. Replies are matched on feature index, function and
    /// software ID; notifications and other programs' replies are skipped.
    pub fn call(&mut self, index: u8, function: u8, params: &[u8], what: Call) -> Result<[u8; 16], Error> {
        assert!(params.len() <= 16 && function < 16);
        // 2..=15: 0 marks notifications and the kernel's hid-logitech-hidpp driver uses 1.
        self.swid = if (2..15).contains(&self.swid) { self.swid + 1 } else { 2 };
        let fsw = function << 4 | self.swid;
        let mut msg = [0u8; 20];
        msg[..4].copy_from_slice(&[LONG, self.device, index, fsw]);
        msg[4..4 + params.len()].copy_from_slice(params);
        self.drain()?;
        self.file.write_all(&msg).map_err(Error::Gone)?;
        let deadline = Instant::now() + REPLY_TIMEOUT;
        while let Some(r) = self.read(deadline)? {
            if !matches!(r[0], 0x10 | LONG) || r[1] != self.device {
                continue;
            }
            self.keep_notification(&r);
            if r[2] == 0xFF && r[3] == index && r[4] == fsw {
                return Err(Error::Device(what, r[5]));
            }
            if r[2] == index && r[3] == fsw {
                let mut out = [0u8; 16];
                out.copy_from_slice(&r[4..20]);
                return Ok(out);
            }
        }
        Err(Error::Timeout(what))
    }

    /// Reads pending input, keeping notifications; fails once the keyboard is gone.
    pub fn drain(&mut self) -> Result<(), Error> {
        while let Some(r) = self.read(Instant::now())? {
            if matches!(r[0], 0x10 | LONG) && r[1] == self.device {
                self.keep_notification(&r);
            }
        }
        Ok(())
    }

    /// Notifications received so far, oldest first.
    pub fn notifications(&mut self) -> Vec<[u8; 20]> {
        self.notes.drain(..).collect()
    }

    fn keep_notification(&mut self, r: &[u8; 20]) {
        // Software ID 0 and not an error report: the device spoke on its own.
        if r[3] & 0x0F == 0 && r[2] != 0xFF {
            if self.notes.len() == 32 {
                self.notes.pop_front(); // bounded: the oldest are the least useful
            }
            self.notes.push_back(*r);
        }
    }

    /// The next input report, or None at the deadline.
    fn read(&mut self, deadline: Instant) -> Result<Option<[u8; 20]>, Error> {
        loop {
            let wait = deadline.saturating_duration_since(Instant::now());
            let mut pfd = libc::pollfd { fd: self.file.as_raw_fd(), events: libc::POLLIN, revents: 0 };
            let ms = wait.as_micros().div_ceil(1000).min(i32::MAX as u128) as i32;
            // SAFETY: one valid pollfd for the duration of the call.
            let n = unsafe { libc::poll(&mut pfd, 1, ms) };
            if n < 0 {
                let e = io::Error::last_os_error();
                match e.kind() {
                    io::ErrorKind::Interrupted => continue,
                    // poll on one valid descriptor fails otherwise only with EFAULT, EINVAL or ENOMEM
                    _ => return Err(Error::Gone(e)), // LCOV_EXCL_LINE
                }
            }
            if n == 0 {
                return Ok(None);
            }
            if pfd.revents & (libc::POLLERR | libc::POLLHUP | libc::POLLNVAL) != 0 {
                return Err(Error::Gone(io::Error::from_raw_os_error(libc::ENODEV)));
            }
            let mut buf = [0u8; 64];
            match self.file.read(&mut buf) {
                Ok(0) => return Err(Error::Gone(io::Error::from_raw_os_error(libc::ENODEV))),
                Ok(n) => {
                    let mut r = [0u8; 20];
                    r[..n.min(20)].copy_from_slice(&buf[..n.min(20)]);
                    return Ok(Some(r));
                }
                Err(e) if matches!(e.kind(), io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted) => {}
                Err(e) => return Err(Error::Gone(e)),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fake;
    use std::thread;

    use crate::testenv;
    use std::fs::File;

    /// A hidraw class directory outside the scratch sysfs, for `identity` alone.
    fn sysfs(name: &str, hid_id: &str, interface: &str) -> PathBuf {
        let class = testenv::init().join("trees").join(name);
        testenv::hid_node(&class, hid_id, interface);
        class
    }

    fn g316() -> &'static Model {
        crate::layout::find("g316-x-98").unwrap()
    }

    #[test]
    fn identity_comes_from_the_hid_and_usb_nodes() {
        let class = sysfs("g316", "0003:0000046D:0000C36B", "02");
        assert_eq!(identity(&class), Some((0x046d, 0xc36b, 2, "Logitech G316 X 98".into())));
        let g316 = g316();
        assert!(is_model(&class, g316));
        assert!(!is_model(&class, crate::layout::find("g903-hero").unwrap()));
        assert!(!is_model(&sysfs("other-if", "0003:0000046D:0000C36B", "01"), g316), "the key-report interface is not ours");
        assert_eq!(identity(&sysfs("bluetooth", "0005:0000046D:0000C36B", "02")), None, "USB only");
        assert_eq!(identity(Path::new("/nonexistent")), None);
    }

    #[test]
    fn replies_errors_and_notifications() {
        let (mut io, fake) = fake::transport(fake::g316(), 0xFF);
        let what = Call { feature: 0, function: 1 };
        assert_eq!(&io.call(0, 1, &[0, 0, 0x5A], what).unwrap()[..3], &[4, 2, 0x5A]);
        // An index the device does not have: its error report comes back as Device.
        let bad = io.call(99, 0, &[], Call { feature: 0x1234, function: 0 }).unwrap_err();
        assert!(matches!(bad, Error::Device(_, 6)));
        assert_eq!(bad.to_string(), "keyboard rejected feature 0x1234 function 0 with HID++ error 0x06");
        // A notification waiting in front of a reply is kept, and the reply still matches.
        fake.notify(13, 0, &[1]);
        assert_eq!(&io.call(0, 1, &[0, 0, 7], what).unwrap()[..3], &[4, 2, 7]);
        let notes = io.notifications();
        assert_eq!(notes.len(), 1);
        assert_eq!(&notes[0][..5], &[0x11, 0xFF, 13, 0x00, 1]);
        assert!(io.notifications().is_empty());
    }

    #[test]
    fn notifications_are_bounded() {
        let (mut io, fake) = fake::transport(fake::g316(), 0xFF);
        for i in 0..40 {
            fake.notify(13, 0, &[i]);
        }
        io.drain().unwrap();
        let notes = io.notifications();
        assert_eq!((notes.len(), notes[0][4], notes[31][4]), (32, 8, 39), "the oldest are dropped");
    }

    #[test]
    fn silence_times_out_and_a_closed_device_is_gone() {
        let (mut io, _peer) = fake::silent(0xFF);
        let what = Call { feature: 0, function: 1 };
        let t = Instant::now();
        assert!(matches!(io.call(0, 1, &[], what), Err(Error::Timeout(_))));
        assert!(t.elapsed() >= REPLY_TIMEOUT);
        let (mut io, fake) = fake::transport(fake::g316(), 0xFF);
        fake.unplug();
        assert!(matches!(io.call(0, 1, &[], what), Err(Error::Gone(_))));
    }

    #[test]
    fn opening_checks_the_node() {
        let g316 = g316();
        assert!(matches!(Hidpp::open(Path::new("/nonexistent/hidraw9"), g316), Err(Error::Access(..))));
        // A regular file is not a character device, so nothing is sent to it.
        let file = testenv::init().join("not-a-device");
        fs::write(&file, "").unwrap();
        assert!(matches!(Hidpp::open_unchecked(&file, 0xFF), Err(Error::WrongDevice(_))));
        assert!(matches!(identify(&file, crate::layout::models()), Err(Error::WrongDevice(_))));
    }

    #[test]
    fn find_and_present_look_for_the_hidpp_interface() {
        let _serial = testenv::serial();
        // The scratch sysfs starts with a G316's key-report interface only.
        assert!(matches!(find(crate::layout::models()), Err(Error::NotFound)));
        assert!(!present(g316()));
        let node = testenv::init().join("sysfs/class/hidraw/hidraw-fake1");
        testenv::hid_node(&node, "0003:0000046D:0000C36B", "02");
        let found = find(crate::layout::models());
        fs::remove_dir_all(&node).unwrap();
        let (path, model) = found.unwrap();
        assert_eq!((path.as_path(), model.id.as_str()), (Path::new("/dev/hidraw-fake1"), "g316-x-98"));
        assert!(!present(g316()), "gone with its node");
    }

    #[test]
    fn a_character_device_is_checked_through_its_device_number() {
        // /dev/null is character device 1:3, which the scratch sysfs makes a G316.
        let null = Path::new("/dev/null");
        assert_eq!(identify(null, crate::layout::models()).unwrap().id, "g316-x-98");
        let g903 = crate::layout::find("g903-hero").unwrap();
        assert!(matches!(Hidpp::open(null, g903), Err(Error::WrongDevice(_))), "not a G903");
        assert!(matches!(identify(Path::new("/dev/zero"), crate::layout::models()), Err(Error::WrongDevice(_))));
        let mut io = Hidpp::open(null, g316()).unwrap();
        assert_eq!(io.identity(null), Some((0x046d, 0xc36b, 2, "Logitech G316 X 98".into())));
        // Reading /dev/null ends at once, the way a hidraw node does once the device is gone.
        assert!(matches!(io.call(0, 1, &[], Call { feature: 0, function: 1 }), Err(Error::Gone(_))));
    }

    #[test]
    fn replies_to_others_are_skipped() {
        let (mut io, peer) = fake::silent(0xFF);
        let device = thread::spawn(move || {
            let mut request = [0u8; 20];
            (&peer).read_exact(&mut request).unwrap();
            let (index, fsw) = (request[2], request[3]);
            let report = |head: [u8; 4]| {
                let mut r = [0u8; 20];
                r[..4].copy_from_slice(&head);
                r[4] = 0x42;
                (&peer).write_all(&r).unwrap();
            };
            report([0x01, 0xFF, index, fsw]); // a keyboard input report
            report([LONG, 0x01, index, fsw]); // another device index
            report([LONG, 0xFF, index + 1, fsw]); // another feature's reply
            report([LONG, 0xFF, index, fsw ^ 0x01]); // another program's software ID
            report([LONG, 0xFF, index, fsw]);
            peer
        });
        let reply = io.call(3, 1, &[], Call { feature: 0x0003, function: 1 }).unwrap();
        assert_eq!(reply[0], 0x42);
        assert!(io.notifications().is_empty(), "replies are not notifications");
        drop(device.join());
    }

    #[test]
    fn a_signal_does_not_cut_the_wait_short() {
        let (mut io, _peer) = fake::silent(0xFF);
        // SAFETY: the calling thread's own ID.
        let me = unsafe { libc::pthread_self() };
        let poke = thread::spawn(move || {
            for _ in 0..3 {
                thread::sleep(Duration::from_millis(200));
                testenv::interrupt(me); // the second wait is surely inside poll on a slow machine too
            }
        });
        let t = Instant::now();
        assert!(matches!(io.call(0, 1, &[], Call { feature: 0, function: 1 }), Err(Error::Timeout(_))));
        assert!(t.elapsed() >= REPLY_TIMEOUT, "the interrupted poll was resumed");
        poke.join().unwrap();
    }

    #[test]
    fn a_failed_read_means_the_device_is_gone() {
        // A directory polls as readable and fails every read.
        let mut io = Hidpp::from_file(File::open(testenv::init()).unwrap(), 0xFF);
        assert!(matches!(io.drain(), Err(Error::Gone(e)) if e.raw_os_error() == Some(libc::EISDIR)));
    }

    #[test]
    fn every_error_explains_itself() {
        let c = Call { feature: 0x8071, function: 5 };
        let io = || io::Error::from_raw_os_error(libc::ENODEV);
        for (e, says) in [
            (Error::NotFound, "no supported keyboard"),
            (Error::Access("/dev/x".into(), io()), "install the udev rule"),
            (Error::WrongDevice("/dev/x".into()), "not the HID++ interface"),
            (Error::Gone(io()), "disconnected"),
            (Error::Timeout(c), "no reply to feature 0x8071 function 5"),
            (Error::Busy(c), "stayed busy"),
            (Error::Unsupported(0x8081), "feature 0x8081"),
            (Error::Reply(c, "short".into()), "unexpected reply"),
            (Error::Refused(c), "outside loginux's write boundary"),
        ] {
            assert!(e.to_string().contains(says), "{e}");
        }
    }
}
