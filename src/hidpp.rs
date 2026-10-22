//! HID++ 2.0 transport over a keyboard's hidraw node.
//!
//! Only the HID++ interface named by a keyboard definition (USB vendor, product
//! and interface number) is opened, and the check is repeated on the opened file
//! descriptor, so a renumbered /dev/hidrawN can never point this at another
//! device. `open_unchecked` exists for `loginux probe` alone, which sends nothing
//! but reads.

use crate::layout::Model;
use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::fs::{FileTypeExt, MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
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

fn nodes() -> Vec<PathBuf> {
    let mut nodes: Vec<PathBuf> =
        fs::read_dir("/sys/class/hidraw").into_iter().flatten().filter_map(|e| Some(e.ok()?.path())).collect();
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
    Ok(PathBuf::from(format!("/sys/dev/char/{major}:{minor}")))
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
        Ok(Hidpp { file, device, swid: 0 })
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

    /// Discards pending input; fails once the keyboard is gone.
    pub fn drain(&mut self) -> Result<(), Error> {
        while self.read(Instant::now())?.is_some() {}
        Ok(())
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
                if e.kind() == io::ErrorKind::Interrupted {
                    continue;
                }
                return Err(Error::Gone(e));
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
