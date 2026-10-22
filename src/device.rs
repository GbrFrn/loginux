//! Typed calls to the HID++ features loginux uses, resolved by feature ID.
//!
//! Write boundary: `ALLOWED` lists every request this program can send. SetEffect
//! always carries persistence 0x01 (apply in RAM, flash-save bit clear) and
//! FrameEnd carries 0x00, so nothing here can ask the keyboard to write its flash.
//! DFU, reset and engineering features are never resolved or called.

use crate::hidpp::{BUSY, Call, Error, Hidpp};
use crate::layout::Model;
use crate::profile::Rgb;
use std::path::{Path, PathBuf};
use std::thread::sleep;
use std::time::{Duration, Instant};

const ROOT: u16 = 0x0000;
const RGB_EFFECTS: u16 = 0x8071;
const PER_KEY: u16 = 0x8081;
const LAYOUT: u16 = 0x4540;

const ALLOWED: &[(u16, u8)] = &[
    (ROOT, 0),        // getFeature
    (ROOT, 1),        // getProtocolVersion
    (RGB_EFFECTS, 0), // getInfo
    (RGB_EFFECTS, 1), // setEffect, RAM only
    (RGB_EFFECTS, 5), // get/set software control
    (PER_KEY, 0),     // getInfo: supported-zone bitmap
    (PER_KEY, 1),     // setIndividualRgbZones: stage colours
    (PER_KEY, 7),     // frameEnd: commit the staged frame
    (LAYOUT, 0),      // getKeyboardLayout
];

const STATIC: u16 = 0x0001; // 0x8071 effect ID
const PERSIST_RAM: u8 = 0x01; // SetEffect persistence: apply now; bit 1 (flash) stays clear
const PAGE: usize = 112; // 0x8081 getInfo pages start at zones 0, 112 and 224
const BUSY_BUDGET: Duration = Duration::from_secs(1);

pub struct Keyboard {
    io: Hidpp,
    pub path: PathBuf,
    pub model: &'static Model,
    rgb: u8,
    per_key: u8,
}

/// What `loginux probe` learns from a hidraw node, using reads only.
pub struct Probe {
    pub vendor: u16,
    pub product: u16,
    pub interface: u8,
    pub hid_name: String,
    pub protocol: (u8, u8),
    pub rgb_effects: bool,
    pub per_key: bool,
    pub layout: Option<u8>,
    pub zones: Vec<u8>,
}

pub fn probe(path: &Path) -> Result<Probe, Error> {
    let io = Hidpp::open_unchecked(path, 0xFF)?;
    let (vendor, product, interface, hid_name) = io.identity(path).ok_or_else(|| Error::WrongDevice(path.into()))?;
    let mut kb = Keyboard { io, path: path.into(), model: &crate::layout::models()[0], rgb: 0, per_key: 0 };
    let protocol = kb.protocol()?;
    let rgb = kb.feature(RGB_EFFECTS)?;
    let per_key = kb.feature(PER_KEY)?;
    let layout = kb.layout_code()?;
    let zones = match per_key {
        Some(index) => {
            kb.per_key = index;
            kb.zones()?
        }
        None => vec![],
    };
    Ok(Probe { vendor, product, interface, hid_name, protocol, rgb_effects: rgb.is_some(), per_key: per_key.is_some(), layout, zones })
}

impl Keyboard {
    pub fn open(path: &Path, model: &'static Model) -> Result<Self, Error> {
        let mut kb = Keyboard { io: Hidpp::open(path, model)?, path: path.into(), model, rgb: 0, per_key: 0 };
        kb.rgb = kb.feature(RGB_EFFECTS)?.ok_or(Error::Unsupported(RGB_EFFECTS))?;
        kb.per_key = kb.feature(PER_KEY)?.ok_or(Error::Unsupported(PER_KEY))?;
        Ok(kb)
    }

    fn feature(&mut self, id: u16) -> Result<Option<u8>, Error> {
        let r = self.call(ROOT, 0, 0, &id.to_be_bytes())?;
        Ok((r[0] != 0).then_some(r[0]))
    }

    fn call(&mut self, feature: u16, index: u8, function: u8, params: &[u8]) -> Result<[u8; 16], Error> {
        let what = Call { feature, function };
        if !ALLOWED.contains(&(feature, function)) {
            return Err(Error::Refused(what));
        }
        self.io.call(index, function, params, what)
    }

    /// Writes wait out BUSY, which SetEffect and FrameEnd return while the previous request is pending.
    fn write(&mut self, feature: u16, index: u8, function: u8, params: &[u8]) -> Result<[u8; 16], Error> {
        let deadline = Instant::now() + BUSY_BUDGET;
        loop {
            match self.call(feature, index, function, params) {
                Err(Error::Device(_, BUSY)) if Instant::now() < deadline => sleep(Duration::from_millis(5)),
                Err(Error::Device(c, BUSY)) => return Err(Error::Busy(c)),
                other => return other,
            }
        }
    }

    /// Fails with `Error::Gone` once the keyboard has been unplugged.
    pub fn alive(&mut self) -> Result<(), Error> {
        self.io.drain()
    }

    pub fn protocol(&mut self) -> Result<(u8, u8), Error> {
        let r = self.call(ROOT, 0, 1, &[0, 0, 0x5A])?;
        Ok((r[0], r[1]))
    }

    /// 0x4540 layout code (0x01 = ANSI), when the keyboard reports the feature.
    pub fn layout_code(&mut self) -> Result<Option<u8>, Error> {
        match self.feature(LAYOUT)? {
            Some(index) => Ok(Some(self.call(LAYOUT, index, 0, &[])?[0])),
            None => Ok(None),
        }
    }

    /// Zone IDs the keyboard accepts, from the three 0x8081 bitmap pages.
    pub fn zones(&mut self) -> Result<Vec<u8>, Error> {
        let mut zones = vec![];
        for page in 0..3u8 {
            let r = self.call(PER_KEY, self.per_key, 0, &[0, page])?;
            if r[1] != page {
                return Err(Error::Reply(Call { feature: PER_KEY, function: 0 }, format!("page {}", r[1])));
            }
            for (byte, bits) in r[2..16].iter().enumerate() {
                for bit in 0..8 {
                    let zone = page as usize * PAGE + byte * 8 + bit;
                    if bits >> bit & 1 == 1 && zone < 0xFF {
                        zones.push(zone as u8); // 0xFF means "skip" in a zone record
                    }
                }
            }
        }
        Ok(zones)
    }

    /// (cluster, effect index) of the Static effect in every lighting cluster.
    pub fn static_effects(&mut self) -> Result<Vec<(u8, u8)>, Error> {
        let clusters = self.call(RGB_EFFECTS, self.rgb, 0, &[0xFF, 0xFF, 0])?[2];
        let mut found = vec![];
        for cluster in 0..clusters {
            let count = self.call(RGB_EFFECTS, self.rgb, 0, &[cluster, 0xFF, 0])?[4];
            let mut index = None;
            for i in 0..count {
                let r = self.call(RGB_EFFECTS, self.rgb, 0, &[cluster, i, 0])?;
                if u16::from_be_bytes([r[2], r[3]]) == STATIC {
                    index = Some(i);
                    break;
                }
            }
            let index = index.ok_or_else(|| {
                Error::Reply(Call { feature: RGB_EFFECTS, function: 0 }, format!("cluster {cluster} has no Static effect"))
            })?;
            found.push((cluster, index));
        }
        Ok(found)
    }

    /// GetSWControl: (mode, flags). Mode bit 0 set = the host owns the lighting effects.
    pub fn sw_control(&mut self) -> Result<(u8, u8), Error> {
        let r = self.call(RGB_EFFECTS, self.rgb, 5, &[0])?;
        Ok((r[1], r[2]))
    }

    pub fn set_sw_control(&mut self, mode: u8, flags: u8) -> Result<(), Error> {
        self.write(RGB_EFFECTS, self.rgb, 5, &[1, mode, flags]).map(drop)
    }

    /// Static colour on one cluster, applied in RAM.
    pub fn set_static(&mut self, (cluster, index): (u8, u8), [r, g, b]: Rgb) -> Result<(), Error> {
        let params = [cluster, index, r, g, b, 0, 0, 0, 0, 0, 0, 0, PERSIST_RAM];
        self.write(RGB_EFFECTS, self.rgb, 1, &params).map(drop)
    }

    /// Stages every (zone, colour) four at a time, then commits the frame.
    pub fn send_frame(&mut self, frame: &[(u8, Rgb)]) -> Result<(), Error> {
        for chunk in frame.chunks(4) {
            let mut params = [0xFFu8; 16];
            for (i, (zone, rgb)) in chunk.iter().enumerate() {
                params[i * 4] = *zone;
                params[i * 4 + 1..i * 4 + 4].copy_from_slice(rgb);
            }
            let echo = self.write(PER_KEY, self.per_key, 1, &params)?;
            if chunk.iter().zip(echo).any(|((zone, _), e)| *zone != e) {
                return Err(Error::Reply(Call { feature: PER_KEY, function: 1 }, format!("echoed {:02x?}", &echo[..4])));
            }
        }
        self.write(PER_KEY, self.per_key, 7, &[0; 5]).map(drop) // FrameEnd, no persistence
    }
}
