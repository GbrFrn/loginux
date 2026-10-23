//! Typed calls to the HID++ features loginux uses, resolved by feature ID.
//!
//! Write boundary: `ALLOWED` lists every request this program can send. SetEffect
//! always carries persistence 0x01 (apply in RAM, flash-save bit clear) and
//! FrameEnd carries 0x00, so no lighting request can ask the keyboard to write
//! its flash; manageNvConfig (0x8071/3) passes only in its get form, because its
//! set form writes EEPROM. The one device setting written is the report rate
//! (0x8061/3), with the 0x8101/6 host-mode switch around it: the rate the user
//! chose, sent when they choose it and again when the keyboard reconnects at
//! another rate. The USB device can re-enumerate at the write. Brightness and
//! game mode are read only. DFU, reset and engineering features are never
//! resolved or called.

use crate::hidpp::{self, BUSY, Call, Error, Hidpp};
use crate::layout::Model;
use crate::profile::Rgb;
use std::path::{Path, PathBuf};
use std::thread::sleep;
use std::time::{Duration, Instant};

const ROOT: u16 = 0x0000;
const RGB_EFFECTS: u16 = 0x8071;
const PER_KEY: u16 = 0x8081;
const LAYOUT: u16 = 0x4540;
const FEATURE_SET: u16 = 0x0001;
const COLOR_LED_EFFECTS: u16 = 0x8070;
const ONBOARD_PROFILES: u16 = 0x8100;
const PROFILE_MANAGEMENT: u16 = 0x8101;
/// 0x8101 function 6 values, as Solaar sends them when it claims a device and
/// when it releases it: host mode, and the mode the keyboard starts in.
const PROFILES_HOST: u8 = 0x05;
const PROFILES_ONBOARD: u8 = 0x03;
const BRIGHTNESS: u16 = 0x8040;
const REPORT_RATE: u16 = 0x8061;
const GAME_MODE: u16 = 0x4523;

const ALLOWED: &[(u16, u8)] = &[
    (ROOT, 0),        // getFeature
    (ROOT, 1),        // getProtocolVersion
    (RGB_EFFECTS, 0), // getInfo
    (RGB_EFFECTS, 1), // setEffect, RAM only
    (RGB_EFFECTS, 5), // get/set software control
    (RGB_EFFECTS, 3), // manageNvConfig: get only, see GET_ONLY
    (PER_KEY, 0),     // getInfo: supported-zone bitmap
    (PER_KEY, 1),     // setIndividualRgbZones: stage colours
    (PER_KEY, 7),     // frameEnd: commit the staged frame
    (LAYOUT, 0),      // getKeyboardLayout
    (BRIGHTNESS, 0),  // getInfo: maximum brightness
    (BRIGHTNESS, 1),  // getBrightness
    (REPORT_RATE, 1), // getReportRateList: supported-rate bitmap
    (REPORT_RATE, 2), // getReportRate
    (REPORT_RATE, 3), // setReportRate: the rate the user chose; a lost reply is read back, not resent
    (PROFILE_MANAGEMENT, 6), // host or onboard mode, around a rate change only; never profile memory
    (GAME_MODE, 0),   // getGameMode
    // `loginux probe` only, all getters:
    (FEATURE_SET, 0),       // getCount
    (FEATURE_SET, 1),       // getFeatureID
    (COLOR_LED_EFFECTS, 0), // getInfo
    (COLOR_LED_EFFECTS, 1), // getZoneInfo
    (COLOR_LED_EFFECTS, 2), // getZoneEffectInfo
    (ONBOARD_PROFILES, 0),  // getInfo
    (ONBOARD_PROFILES, 2),  // getOnboardMode
];

/// Functions whose first parameter picks get (0x00) or set, where the set form
/// writes non-volatile memory: only the get form passes.
const GET_ONLY: &[(u16, u8)] = &[(RGB_EFFECTS, 3)];

/// Names of common HID++ 2.0 features, after Solaar's table; for `loginux probe`.
const FEATURE_NAMES: &[(u16, &str)] = &[
    (0x0000, "IRoot"), (0x0001, "IFeatureSet"), (0x0002, "IFeatureInfo"), (0x0003, "DeviceFwVersion"),
    (0x0005, "DeviceNameType"), (0x0007, "DeviceFriendlyName"), (0x0020, "ConfigChange"), (0x0021, "UniqueRandomId"),
    (0x00c2, "DfuControlSigned"), (0x00c3, "DfuControl"), (0x00d0, "Dfu"), (0x1000, "BatteryStatus"),
    (0x1001, "BatteryVoltage"), (0x1004, "UnifiedBattery"), (0x1300, "LedControl"), (0x1802, "DeviceReset"),
    (0x1805, "OobState"), (0x1806, "ConfigDeviceProps"), (0x1814, "ChangeHost"), (0x1815, "HostsInfo"),
    (0x1981, "Backlight"), (0x1b04, "ReprogControlsV4"), (0x1d4b, "WirelessDeviceStatus"), (0x2100, "VerticalScrolling"),
    (0x2120, "HiResScroll"), (0x2121, "HiResWheel"), (0x2201, "AdjustableDpi"), (0x2202, "ExtendedAdjustableDpi"),
    (0x4523, "GameMode"), (0x4540, "KeyboardLayout"), (0x8010, "GKey"), (0x8020, "MKeys"), (0x8030, "MacroRecord"),
    (0x8040, "BrightnessControl"), (0x8060, "AdjustableReportRate"), (0x8061, "ExtendedAdjustableReportRate"),
    (0x8070, "ColorLedEffects"), (0x8071, "RgbEffects"), (0x8080, "PerKeyLighting"), (0x8081, "PerKeyLightingV2"),
    (0x8090, "ModeStatus"), (0x8100, "OnboardProfiles"), (0x8110, "MouseButtonSpy"), (0x8111, "LatencyMonitoring"),
];

pub fn feature_name(id: u16) -> &'static str {
    FEATURE_NAMES.iter().find(|(f, _)| *f == id).map_or("", |(_, n)| n)
}

/// 0x8061 rate index to rate: bit `i` of the supported bitmap is index `i`.
pub const RATES_HZ: [u32; 7] = [125, 250, 500, 1000, 2000, 4000, 8000];

/// Optional HID++ features: None when the keyboard does not report them.
#[derive(Clone, Copy, Default)]
struct Optional {
    brightness: Option<u8>,
    report_rate: Option<u8>,
    game_mode: Option<u8>,
    profiles: Option<u8>,
}

/// Something the keyboard reported on its own, outside a request.
pub enum Event {
    GameMode(bool),
    /// 0x8040 reported a change made on the keyboard (its knob or keys): the
    /// brightness, or the lighting turned on or off. Read it again.
    Brightness,
}

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
    optional: Optional,
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
    /// 0x8071 clusters: (index, event-display modes). Modes other than 0 mark a
    /// status display, such as a G903's DPI LEDs, whose firmware picks the LEDs.
    pub clusters: Vec<(u8, u8)>,
    /// (index, feature ID, type flags, version) from IFeatureSet.
    pub features: Vec<(u8, u16, u8, u8)>,
    /// Lighting getters, as labelled raw replies for research notes.
    pub lighting: Vec<String>,
}

pub fn probe(path: &Path) -> Result<Probe, Error> {
    #[cfg(test)]
    if let Some((io, m)) = crate::fake::plugged(path) {
        return probe_on(io, path, (m.vendor, m.product, m.interface, m.name.clone()));
    }
    let io = Hidpp::open_unchecked(path, 0xFF)?;
    let identity = io.identity(path).ok_or_else(|| Error::WrongDevice(path.into()))?;
    probe_on(io, path, identity)
}

/// The reads behind `probe`, on an opened transport with its sysfs identity.
fn probe_on(io: Hidpp, path: &Path, identity: (u16, u16, u8, String)) -> Result<Probe, Error> {
    let (vendor, product, interface, hid_name) = identity;
    // The model is unused by these reads; any definition will do.
    let mut kb = Keyboard {
        io,
        path: path.into(),
        model: &crate::layout::models()[0],
        rgb: 0,
        per_key: 0,
        optional: Optional::default(),
    };
    let protocol = kb.protocol()?;
    let rgb = kb.feature(RGB_EFFECTS)?;
    let per_key = kb.feature(PER_KEY)?;
    let layout = kb.layout_code()?;
    let mut features = vec![];
    if let Some(set) = kb.feature(FEATURE_SET)? {
        let count = kb.call(FEATURE_SET, set, 0, &[])?[0];
        for i in 1..=count {
            let r = kb.call(FEATURE_SET, set, 1, &[i])?;
            features.push((i, u16::from_be_bytes([r[0], r[1]]), r[2], r[3]));
        }
    }
    let hex = |r: &[u8]| r.iter().map(|b| format!("{b:02x}")).collect::<Vec<_>>().join(" ");
    let mut lighting = vec![];
    let mut clusters = vec![];
    if let Some(index) = rgb {
        // 0x8071 getInfo: clusters, then each cluster's effects (ID, capabilities, period).
        let info = kb.call(RGB_EFFECTS, index, 0, &[0xFF, 0xFF, 0])?;
        lighting.push(format!("0x8071 info: {}", hex(&info[..8])));
        for cluster in 0..info[2] {
            let c = kb.call(RGB_EFFECTS, index, 0, &[cluster, 0xFF, 0])?;
            lighting.push(format!("0x8071 cluster {cluster}: {}", hex(&c[..8])));
            clusters.push((cluster, c[5]));
            for e in 0..c[4] {
                let r = kb.call(RGB_EFFECTS, index, 0, &[cluster, e, 0])?;
                lighting.push(format!("0x8071 cluster {cluster} effect {e}: {}", hex(&r[..10])));
            }
        }
        // Effects the device can run on all clusters at once (cluster 0xFF).
        for e in 0..info[7] {
            let r = kb.call(RGB_EFFECTS, index, 0, &[0xFF, e, 0])?;
            lighting.push(format!("0x8071 all clusters effect {e}: {}", hex(&r[..10])));
        }
        // manageNvConfig get for each non-volatile capability the device lists;
        // 0x0008 eventsDisplay decides when DPI-status LEDs light.
        let nv = u16::from_be_bytes([info[3], info[4]]);
        for cap in (0..16).map(|bit| 1u16 << bit).filter(|cap| nv & cap != 0) {
            let [hi, lo] = cap.to_be_bytes();
            let line = match kb.call(RGB_EFFECTS, index, 3, &[0, hi, lo]) {
                Ok(r) => hex(&r[..10]),
                Err(e) => e.to_string(),
            };
            lighting.push(format!("0x8071 nv config {cap:#06x}: {line}"));
        }
    }
    if let Some(index) = kb.feature(COLOR_LED_EFFECTS)? {
        // 0x8070: getInfo, getZoneInfo(zone), getZoneEffectInfo(zone, effect).
        let info = kb.call(COLOR_LED_EFFECTS, index, 0, &[])?;
        lighting.push(format!("0x8070 info: {}", hex(&info[..8])));
        for zone in 0..info[0] {
            let z = kb.call(COLOR_LED_EFFECTS, index, 1, &[zone, 0xFF, 0])?;
            lighting.push(format!("0x8070 zone {zone}: {}", hex(&z[..8])));
            for e in 0..z[3] {
                let r = kb.call(COLOR_LED_EFFECTS, index, 2, &[zone, e, 0])?;
                lighting.push(format!("0x8070 zone {zone} effect {e}: {}", hex(&r[..10])));
            }
        }
    }
    if let Some(index) = kb.feature(ONBOARD_PROFILES)? {
        lighting.push(format!("0x8100 info: {}", hex(&kb.call(ONBOARD_PROFILES, index, 0, &[])?[..16])));
        lighting.push(format!("0x8100 onboard mode: {}", hex(&kb.call(ONBOARD_PROFILES, index, 2, &[])?[..4])));
    }
    let zones = match per_key {
        Some(index) => {
            kb.per_key = index;
            kb.zones()?
        }
        None => vec![],
    };
    Ok(Probe {
        vendor,
        product,
        interface,
        hid_name,
        protocol,
        rgb_effects: rgb.is_some(),
        per_key: per_key.is_some(),
        layout,
        zones,
        clusters,
        features,
        lighting,
    })
}

impl Keyboard {
    /// The keyboard at `path`, or else the plugged-in device of `model`. Without a
    /// path no other definition is tried, so a mouse plugged in beside the
    /// keyboard is never taken for it while the keyboard is away.
    pub fn find(path: Option<&Path>, model: &'static Model) -> Result<Self, Error> {
        #[cfg(test)]
        if let Some(path) = path
            && let Some((io, model)) = crate::fake::plugged(path)
        {
            return Self::attach(io, path, model);
        }
        let (path, model) = match path {
            Some(path) => (path.to_path_buf(), hidpp::identify(path, crate::layout::models())?),
            None => hidpp::find(std::slice::from_ref(model))?,
        };
        Self::open(&path, model)
    }

    pub fn open(path: &Path, model: &'static Model) -> Result<Self, Error> {
        Self::attach(Hidpp::open(path, model)?, path, model)
    }

    /// Resolves the features of `model` on an opened transport.
    pub(crate) fn attach(io: Hidpp, path: &Path, model: &'static Model) -> Result<Self, Error> {
        let mut kb = Keyboard { io, path: path.into(), model, rgb: 0, per_key: 0, optional: Optional::default() };
        kb.rgb = kb.feature(RGB_EFFECTS)?.ok_or(Error::Unsupported(RGB_EFFECTS))?;
        if model.per_key {
            kb.per_key = kb.feature(PER_KEY)?.ok_or(Error::Unsupported(PER_KEY))?;
        }
        kb.optional = Optional {
            brightness: kb.feature(BRIGHTNESS)?,
            report_rate: kb.feature(REPORT_RATE)?,
            game_mode: kb.feature(GAME_MODE)?,
            profiles: kb.feature(PROFILE_MANAGEMENT)?,
        };
        Ok(kb)
    }

    fn feature(&mut self, id: u16) -> Result<Option<u8>, Error> {
        let r = self.call(ROOT, 0, 0, &id.to_be_bytes())?;
        Ok((r[0] != 0).then_some(r[0]))
    }

    fn call(&mut self, feature: u16, index: u8, function: u8, params: &[u8]) -> Result<[u8; 16], Error> {
        let what = Call { feature, function };
        let set_form = GET_ONLY.contains(&(feature, function)) && params.first() != Some(&0);
        if !ALLOWED.contains(&(feature, function)) || set_form {
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

    /// Effect IDs each cluster lists, in effect-index order.
    pub fn effect_ids(&mut self) -> Result<Vec<Vec<u16>>, Error> {
        let clusters = self.call(RGB_EFFECTS, self.rgb, 0, &[0xFF, 0xFF, 0])?[2];
        let mut all = vec![];
        for cluster in 0..clusters {
            let count = self.call(RGB_EFFECTS, self.rgb, 0, &[cluster, 0xFF, 0])?[4];
            let mut ids = vec![];
            for i in 0..count {
                let r = self.call(RGB_EFFECTS, self.rgb, 0, &[cluster, i, 0])?;
                ids.push(u16::from_be_bytes([r[2], r[3]]));
            }
            all.push(ids);
        }
        Ok(all)
    }

    /// IDs of the effects the device can play on all its clusters at once
    /// (cluster 0xFF), in effect-index order. One such effect runs on one clock,
    /// so the clusters stay in step.
    pub fn multi_cluster_ids(&mut self) -> Result<Vec<u16>, Error> {
        let count = self.call(RGB_EFFECTS, self.rgb, 0, &[0xFF, 0xFF, 0])?[7];
        let mut ids = vec![];
        for i in 0..count {
            let r = self.call(RGB_EFFECTS, self.rgb, 0, &[0xFF, i, 0])?;
            ids.push(u16::from_be_bytes([r[2], r[3]]));
        }
        Ok(ids)
    }

    /// Any listed effect on one cluster, or on all of them (0xFF), applied in RAM (persistence 0x01).
    pub fn set_effect(&mut self, cluster: u8, index: u8, params: [u8; 10]) -> Result<(), Error> {
        let mut msg = [0u8; 13];
        msg[0] = cluster;
        msg[1] = index;
        msg[2..12].copy_from_slice(&params);
        msg[12] = PERSIST_RAM;
        self.write(RGB_EFFECTS, self.rgb, 1, &msg).map(drop)
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

    /// 0x8040: (current, maximum) brightness.
    pub fn brightness(&mut self) -> Result<Option<(u16, u16)>, Error> {
        let Some(index) = self.optional.brightness else { return Ok(None) };
        let info = self.call(BRIGHTNESS, index, 0, &[])?;
        let now = self.call(BRIGHTNESS, index, 1, &[])?;
        Ok(Some((u16::from_be_bytes([now[0], now[1]]), u16::from_be_bytes([info[0], info[1]]))))
    }

    /// 0x8061: (supported rate indices, current index).
    pub fn report_rates(&mut self) -> Result<Option<(Vec<u8>, u8)>, Error> {
        let Some(index) = self.optional.report_rate else { return Ok(None) };
        let list = self.call(REPORT_RATE, index, 1, &[0])?;
        let bits = u16::from_be_bytes([list[0], list[1]]);
        let supported = (0..RATES_HZ.len() as u8).filter(|i| bits >> i & 1 == 1).collect();
        let current = self.call(REPORT_RATE, index, 2, &[0])?[0];
        Ok(Some((supported, current)))
    }

    /// 0x8061/3, sent once, for the rate the user chose: the keyboard can
    /// re-enumerate before it answers, so a lost reply means "unknown", and the
    /// caller reads the rate back instead of sending it again.
    pub fn set_report_rate(&mut self, rate_index: u8) -> Result<(), Error> {
        let index = self.optional.report_rate.ok_or(Error::Unsupported(REPORT_RATE))?;
        // With 0x8101 profile management the write is refused (HID++ error 2) until
        // the host takes over, as G HUB does before its rate writes. Control goes
        // straight back to the firmware, as Solaar does on release; the new rate
        // stays at least until the keyboard reconnects.
        let profiles = self.optional.profiles;
        if let Some(p) = profiles {
            self.call(PROFILE_MANAGEMENT, p, 6, &[PROFILES_HOST])?;
        }
        let sent = self.call(REPORT_RATE, index, 3, &[rate_index]).map(drop);
        if let Some(p) = profiles {
            let _ = self.call(PROFILE_MANAGEMENT, p, 6, &[PROFILES_ONBOARD]); // fails harmlessly while re-enumerating
        }
        sent
    }

    /// 0x4523: game mode on or off.
    pub fn game_mode(&mut self) -> Result<Option<bool>, Error> {
        let Some(index) = self.optional.game_mode else { return Ok(None) };
        Ok(Some(self.call(GAME_MODE, index, 0, &[])?[0] != 0))
    }

    /// Notifications that arrived since the last call, typed where they are known.
    pub fn events(&mut self) -> Vec<Event> {
        let (game, brightness) = (self.optional.game_mode, self.optional.brightness);
        self.io
            .notifications()
            .into_iter()
            .filter_map(|r| match Some(r[2]) {
                i if i == game && r[3] >> 4 == 0 => Some(Event::GameMode(r[4] != 0)),
                i if i == brightness => Some(Event::Brightness),
                _ => None,
            })
            .collect()
    }

}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fake;

    #[test]
    fn reads_everything_the_engine_needs_from_a_g316() {
        let (mut kb, _fake) = fake::keyboard("g316-x-98", fake::g316());
        assert_eq!(kb.protocol().unwrap(), (4, 2));
        assert_eq!(kb.layout_code().unwrap(), Some(1));
        let mut want: Vec<u8> = kb.model.zones.iter().map(|z| z.id).collect();
        want.sort_unstable();
        assert_eq!(kb.zones().unwrap(), want);
        assert_eq!(kb.static_effects().unwrap(), vec![(0, 1), (1, 1)]);
        assert_eq!(kb.effect_ids().unwrap()[1], vec![0x0000, 0x0001, 0x000a, 0x0015, 0x0016]);
        assert_eq!(kb.brightness().unwrap(), Some((100, 100)));
        assert_eq!(kb.report_rates().unwrap(), Some((vec![3, 4, 5, 6], 3)));
        assert_eq!(kb.game_mode().unwrap(), Some(false));
    }

    #[test]
    fn effects_need_software_control_and_stay_in_ram() {
        let (mut kb, fake) = fake::keyboard("g316-x-98", fake::g316());
        let refused = kb.set_static((0, 1), [1, 2, 3]).unwrap_err();
        assert!(matches!(refused, Error::Device(_, 5)), "{refused}");
        kb.set_sw_control(1, 0).unwrap();
        assert_eq!(kb.sw_control().unwrap(), (1, 0));
        kb.set_static((0, 1), [1, 2, 3]).unwrap();
        kb.set_effect(1, 4, [0, 255, 0, 0, 0, 0, 0x0b, 0xb8, 100, 1]).unwrap();
        let st = fake.state();
        assert_eq!(st.effects[&0], (1, [1, 2, 3, 0, 0, 0, 0, 0, 0, 0], 0x01));
        assert_eq!(st.effects[&1].2, 0x01, "persistence: RAM, never flash");
    }

    #[test]
    fn frames_go_four_zones_per_report_then_one_frame_end() {
        let (mut kb, fake) = fake::keyboard("g316-x-98", fake::g316());
        let frame: Vec<(u8, Rgb)> = (1..=9).map(|z| (z, [z, 0, 0])).collect();
        kb.send_frame(&frame).unwrap();
        let st = fake.state();
        assert_eq!((st.frames, st.shown.len(), st.shown[&9]), (1, 9, [9, 0, 0]));
        assert_eq!(st.log.iter().filter(|r| r.0 == PER_KEY && r.1 == 1).count(), 3);
    }

    #[test]
    fn report_rate_changes_and_refusals() {
        let (mut kb, fake) = fake::keyboard("g316-x-98", fake::g316());
        kb.set_report_rate(4).unwrap();
        assert_eq!(fake.state().rate, 4);
        fake.state().refuse_rate = Some((6, 2));
        let refused = kb.set_report_rate(6).unwrap_err();
        assert!(matches!(refused, Error::Device(Call { feature: REPORT_RATE, function: 3 }, 2)));
        assert_eq!(fake.state().rate, 4);
        // The request is G HUB's: function 3 with the rate index as its one byte.
        let sets: Vec<Vec<u8>> = fake.requests(REPORT_RATE).into_iter().filter(|r| r.0 == 3).map(|r| r.1).collect();
        assert_eq!(sets.len(), 2, "sent once each, never retried");
        for (set, index) in sets.iter().zip([4u8, 6]) {
            assert_eq!(set[0], index);
            assert!(set[1..].iter().all(|&b| b == 0), "only the index: {set:02x?}");
        }
    }

    #[test]
    fn the_write_boundary_refuses_before_anything_is_sent() {
        let (mut kb, fake) = fake::keyboard("g316-x-98", fake::g316());
        let (rgb, per_key) = (kb.rgb, kb.per_key);
        // manageNvConfig: the get form passes, the set form (an EEPROM write) never leaves.
        assert!(kb.call(RGB_EFFECTS, rgb, 3, &[0, 0x00, 0x01]).is_ok());
        assert!(matches!(kb.call(RGB_EFFECTS, rgb, 3, &[1, 0x00, 0x01, 2]), Err(Error::Refused(_))));
        // Functions and features nobody listed: 0x8081 function 6, firmware update.
        assert!(matches!(kb.call(PER_KEY, per_key, 6, &[]), Err(Error::Refused(_))));
        assert!(matches!(kb.call(0x00c3, 9, 0, &[]), Err(Error::Refused(_))));
        assert!(fake.requests(RGB_EFFECTS).iter().all(|(f, p)| !(*f == 3 && p[0] != 0)), "an NV write reached the device");
        assert!(fake.requests(PER_KEY).iter().all(|(f, _)| *f != 6));
        assert!(fake.requests(0x00c3).is_empty());
    }

    #[test]
    fn game_mode_notifications_become_events() {
        let (mut kb, fake) = fake::keyboard("g316-x-98", fake::g316());
        fake.notify(kb.optional.game_mode.unwrap(), 0, &[1]);
        kb.alive().unwrap();
        assert!(matches!(kb.events()[..], [Event::GameMode(true)]));
        assert!(kb.events().is_empty(), "each notification is handed out once");
    }

    #[test]
    fn brightness_notifications_ask_for_a_new_read_and_others_are_dropped() {
        let (mut kb, fake) = fake::keyboard("g316-x-98", fake::g316());
        let brightness = kb.optional.brightness.unwrap();
        fake.notify(brightness, 0, &[0, 50]); // the knob set 50
        fake.notify(brightness, 1, &[0]); // the lighting went off
        fake.notify(kb.optional.report_rate.unwrap(), 0, &[6]); // nothing Loginux follows
        kb.alive().unwrap();
        assert!(matches!(kb.events()[..], [Event::Brightness, Event::Brightness]));
    }

    #[test]
    fn an_unplugged_device_is_gone() {
        let (mut kb, fake) = fake::keyboard("g316-x-98", fake::g316());
        fake.unplug();
        assert!(matches!(kb.alive(), Err(Error::Gone(_))));
    }

    #[test]
    fn the_g903_has_clusters_and_no_optional_controls() {
        let (mut kb, _fake) = fake::keyboard("g903-hero", fake::g903());
        assert_eq!(kb.layout_code().unwrap(), None);
        assert_eq!(kb.static_effects().unwrap(), vec![(0, 1), (1, 1)]);
        assert_eq!((kb.report_rates().unwrap(), kb.brightness().unwrap(), kb.game_mode().unwrap()), (None, None, None));
        assert!(matches!(kb.set_report_rate(3), Err(Error::Unsupported(REPORT_RATE))));
    }

    #[test]
    fn a_per_key_definition_needs_per_key_lighting() {
        let model = crate::layout::find("g316-x-98").unwrap();
        let (io, _fake) = fake::transport(fake::g903(), 0xFF);
        assert!(matches!(Keyboard::attach(io, Path::new("/dev/fake"), model), Err(Error::Unsupported(PER_KEY))));
    }

    #[test]
    fn probe_reads_a_g903_like_the_live_probe() {
        let (io, fake) = fake::transport(fake::g903(), 0xFF);
        let p = probe_on(io, Path::new("/dev/fake"), (0x046d, 0xc091, 2, "G903".into())).unwrap();
        assert_eq!((p.vendor, p.product, p.interface, p.protocol), (0x046d, 0xc091, 2, (4, 2)));
        assert!(p.rgb_effects && !p.per_key && p.zones.is_empty() && p.layout.is_none());
        assert_eq!(p.clusters, [(0, 0x07), (1, 0)], "cluster 0 is the DPI status display");
        assert_eq!((p.features.len(), p.features[6]), (11, (7, 0x8071, 0, 0)));
        let lines = p.lighting.join("\n");
        for want in [
            "0x8071 info: ff 00 02 00 0b 00 04 00",
            "0x8071 cluster 0: 00 00 00 01 04 07 00 00",
            "0x8071 cluster 1 effect 2: 01 02 00 03 00 00 00 00 00 00",
            "0x8071 nv config 0x0008: 00 00 08 01 07 00 00 00 00 00",
            "0x8100 onboard mode: 01 00 00 00",
        ] {
            assert!(lines.contains(want), "{want} missing from\n{lines}");
        }
        assert!(fake.requests(RGB_EFFECTS).iter().all(|(f, p)| !(*f == 3 && p[0] != 0)));
    }

    #[test]
    fn probe_reads_the_g316_zones() {
        let (io, _fake) = fake::transport(fake::g316(), 0xFF);
        let p = probe_on(io, Path::new("/dev/fake"), (0x046d, 0xc36b, 2, "G316".into())).unwrap();
        assert!(p.per_key && p.rgb_effects);
        assert_eq!((p.zones.len(), p.layout), (133, Some(1)));
        assert!(p.lighting.iter().any(|l| l.starts_with("0x8071 nv config 0x0040")));
    }

    #[test]
    fn probe_reads_an_older_colour_led_device() {
        // No feature set and no 0x8071: 0x8070 colour LED effects only.
        let spec = fake::Spec { features: vec![0x0000, 0x0003, 0x8070], clusters: vec![], nv: vec![], ext_caps: 0, multi: vec![], rate_needs_host: false, zones: vec![] };
        let (io, _fake) = fake::transport(spec, 0xFF);
        let p = probe_on(io, Path::new("/dev/fake"), (0x046d, 0xc33f, 1, "Old".into())).unwrap();
        assert!(p.features.is_empty() && !p.rgb_effects && !p.per_key && p.zones.is_empty());
        assert_eq!(
            p.lighting,
            [
                "0x8070 info: 01 00 00 00 00 00 00 00",
                "0x8070 zone 0: 00 00 01 02 00 00 00 00",
                "0x8070 zone 0 effect 0: 00 00 00 01 00 00 00 00 00 00",
                "0x8070 zone 0 effect 1: 00 01 00 02 00 00 00 00 00 00",
            ]
        );
    }

    #[test]
    fn probe_notes_a_refused_nv_read_and_goes_on() {
        let (io, fake) = fake::transport(fake::g903(), 0xFF);
        fake.state().refuse = Some(Box::new(|feature, function, _| ((feature, function) == (RGB_EFFECTS, 3)).then_some(2)));
        let p = probe_on(io, Path::new("/dev/fake"), (0x046d, 0xc091, 2, "G903".into())).unwrap();
        let refused = "0x8071 nv config 0x0001: keyboard rejected feature 0x8071 function 3 with HID++ error 0x02";
        assert!(p.lighting.iter().any(|l| l == refused), "{:?}", p.lighting);
        assert!(p.lighting.last().unwrap().starts_with("0x8100 onboard mode"));
    }

    #[test]
    fn opening_a_path_checks_its_sysfs_identity_first() {
        // The scratch sysfs makes /dev/null a G316, which then reads as unplugged;
        // /dev/zero has no sysfs identity at all.
        assert!(matches!(probe(Path::new("/dev/null")), Err(Error::Gone(_))));
        assert!(matches!(probe(Path::new("/dev/zero")), Err(Error::WrongDevice(_))));
        let g316 = crate::layout::find("g316-x-98").unwrap();
        assert!(matches!(Keyboard::open(Path::new("/dev/null"), g316), Err(Error::Gone(_))));
        assert!(matches!(Keyboard::find(Some(Path::new("/dev/null")), g316), Err(Error::Gone(_))));
        assert!(matches!(Keyboard::find(Some(Path::new("/nonexistent/hidraw9")), g316), Err(Error::Access(..))));
    }

    #[test]
    fn without_a_path_only_the_given_model_is_looked_for() {
        let _serial = crate::testenv::serial();
        let (g316, g903) = (crate::layout::find("g316-x-98").unwrap(), crate::layout::find("g903-hero").unwrap());
        // A G903 is plugged in, the G316 is not: the mouse is not taken for the keyboard.
        let mouse = crate::testenv::init().join("sysfs/class/hidraw/hidraw-fake3");
        crate::testenv::hid_node(&mouse, "0003:0000046D:0000C091", "02");
        let keyboard = Keyboard::find(None, g316).err();
        let found = Keyboard::find(None, g903).err();
        std::fs::remove_dir_all(&mouse).unwrap();
        assert!(matches!(keyboard, Some(Error::NotFound)));
        assert!(matches!(found, Some(Error::Access(path, _)) if path == Path::new("/dev/hidraw-fake3")), "found, then opened");
    }

    #[test]
    fn writes_wait_out_busy_but_not_forever() {
        let (mut kb, fake) = fake::keyboard("g316-x-98", fake::g316());
        let mut busy = 3;
        fake.state().refuse = Some(Box::new(move |feature, function, _| {
            let refuse = (feature, function) == (PER_KEY, 7) && busy > 0;
            busy -= usize::from(refuse);
            refuse.then_some(BUSY)
        }));
        kb.send_frame(&[(4, [1, 2, 3])]).unwrap();
        assert_eq!(fake.requests(PER_KEY).iter().filter(|r| r.0 == 7).count(), 4, "three BUSY answers, then the frame");
        fake.state().refuse = Some(Box::new(|feature, function, _| ((feature, function) == (PER_KEY, 7)).then_some(BUSY)));
        let t = Instant::now();
        assert!(matches!(kb.send_frame(&[(4, [1, 2, 3])]), Err(Error::Busy(_))));
        assert!(t.elapsed() >= BUSY_BUDGET);
    }

    #[test]
    fn odd_replies_are_errors() {
        let (mut kb, fake) = fake::keyboard("g316-x-98", fake::g316());
        fake.state().mangle = Some(|feature, function, r| {
            if (feature, function) == (PER_KEY, 0) {
                r[1] ^= 1; // another page than asked for
            }
        });
        assert!(matches!(kb.zones(), Err(Error::Reply(_, why)) if why == "page 1"));
        fake.state().mangle = Some(|feature, function, r| {
            if (feature, function) == (PER_KEY, 1) {
                r[0] ^= 1; // another zone than sent
            }
        });
        assert!(matches!(kb.send_frame(&[(4, [1, 2, 3])]), Err(Error::Reply(_, why)) if why.starts_with("echoed [05")));
        let mut spec = fake::g903();
        spec.clusters[1].2.retain(|&id| id != STATIC);
        let (mut mouse, _fake) = fake::keyboard("g903-hero", spec);
        assert!(matches!(mouse.static_effects(), Err(Error::Reply(_, why)) if why == "cluster 1 has no Static effect"));
    }

    #[test]
    fn a_rate_change_takes_host_mode_for_the_write_only() {
        let (mut kb, fake) = fake::keyboard("g316-x-98", fake::g316());
        let index = kb.optional.report_rate.unwrap();
        // Written straight, outside host mode: refused.
        assert!(matches!(kb.call(REPORT_RATE, index, 3, &[4]), Err(Error::Device(_, 2))));
        kb.set_report_rate(4).unwrap();
        assert_eq!(fake.state().rate, 4);
        assert!(!fake.state().profiles_host, "handed back to the firmware");
        let order: Vec<(u16, u8, u8)> = fake
            .state()
            .log
            .iter()
            .filter(|r| r.0 == PROFILE_MANAGEMENT || (r.0 == REPORT_RATE && r.1 == 3))
            .map(|r| (r.0, r.1, r.2[0]))
            .collect();
        let expected = [
            (REPORT_RATE, 3, 4), // the refused direct write
            (PROFILE_MANAGEMENT, 6, PROFILES_HOST),
            (REPORT_RATE, 3, 4),
            (PROFILE_MANAGEMENT, 6, PROFILES_ONBOARD),
        ];
        assert_eq!(order, expected);
    }

    #[test]
    fn a_keyboard_without_profile_management_gets_no_profile_request() {
        let mut spec = fake::g316();
        spec.features.retain(|&f| f != PROFILE_MANAGEMENT);
        spec.rate_needs_host = false;
        let (mut kb, fake) = fake::keyboard("g316-x-98", spec);
        kb.set_report_rate(5).unwrap();
        assert_eq!(fake.state().rate, 5);
        assert!(fake.requests(PROFILE_MANAGEMENT).is_empty());
    }

    #[test]
    fn feature_names_and_rates() {
        assert_eq!((feature_name(0x8071), feature_name(0x8081), feature_name(0x1234)), ("RgbEffects", "PerKeyLightingV2", ""));
        assert_eq!(RATES_HZ[3], 1000);
        assert_eq!(RATES_HZ[6], 8000);
    }
}
