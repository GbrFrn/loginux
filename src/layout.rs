//! Device definitions: which USB device is which keyboard or mouse, and where
//! its lighting zones are. Built-in definitions live in `devices/` and are
//! compiled in; more can be added without rebuilding as
//! `$XDG_CONFIG_HOME/loginux/devices/ID.toml`, where a file with a built-in's ID
//! replaces it. CONTRIBUTING.md describes the format.
//!
//! Positions are layout data in key units (1 = one key pitch), not proof of
//! which LED lights; a channel check on the keyboard is the proof.

use serde::Deserialize;
use std::collections::HashSet;
use std::sync::OnceLock;

/// Add a line here when a definition joins `devices/`.
const BUILT_IN: &[(&str, &str)] = &[
    ("g316-x-98", include_str!("../devices/g316-x-98.toml")),
    ("g903-hero", include_str!("../devices/g903-hero.toml")),
];

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Kind {
    Key,
    /// One segment of a light strip, such as the G316's light bar.
    Strip,
    /// Accepted by the keyboard with no known physical effect; lit only by number.
    Unknown,
}

pub struct Zone {
    pub id: u8,
    /// Profile name: a key name, or a strip name plus its segment number. Empty for unknown zones.
    pub name: String,
    pub label: String,
    /// Drawn instead of `label` when the label does not fit the key; never a prefix that names another key.
    pub short: Option<String>,
    pub kind: Kind,
    /// Base-colour group: "keys", the strip's name, or empty for unknown zones.
    pub group: String,
    /// x, y, width, height in key units; zero for unknown zones.
    pub rect: [f32; 4],
}

pub struct Strip {
    pub name: String,
    pub title: String,
    pub zones: Vec<u8>,
}

pub struct Model {
    pub id: String,
    pub name: String,
    /// true: zones are 0x8081 per-key zones painted in frames. false: zones are
    /// 0x8071 clusters, each painted with the Static effect.
    pub per_key: bool,
    pub vendor: u16,
    pub product: u16,
    pub interface: u8,
    /// HID++ device index: 0xff for a keyboard on its own USB cable.
    pub device_index: u8,
    pub board: [f32; 2],
    pub zones: Vec<Zone>,
    pub strips: Vec<Strip>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Def {
    name: String,
    protocol: String,
    #[serde(default = "per_key")]
    lighting: String,
    vendor: u16,
    product: u16,
    interface: u8,
    #[serde(default = "wired")]
    device_index: u8,
    board: [f32; 2],
    #[serde(default)]
    unknown: Vec<u8>,
    #[serde(default)]
    keys: Vec<KeyDef>,
    /// Named lights that are not keys, such as a mouse logo; `zone` is the
    /// cluster on a "clusters" device.
    #[serde(default)]
    lights: Vec<KeyDef>,
    #[serde(default)]
    strip: Vec<StripDef>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct KeyDef {
    name: String,
    zone: u8,
    label: Option<String>,
    short: Option<String>,
    at: [f32; 2],
    #[serde(default = "one_unit")]
    size: [f32; 2],
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct StripDef {
    name: String,
    title: String,
    rect: [f32; 4],
    zones: Vec<u8>,
}

fn wired() -> u8 {
    0xFF
}

fn per_key() -> String {
    "per-key".into()
}

fn one_unit() -> [f32; 2] {
    [1.0, 1.0]
}

/// Names a profile may give a key or strip: lowercase letters, digits and '_'.
fn valid_name(name: &str) -> bool {
    !name.is_empty() && name.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
}

impl Model {
    pub fn parse(id: &str, text: &str) -> Result<Model, String> {
        let at = |e: String| format!("keyboard definition {id}: {e}");
        let d: Def = toml::from_str(text).map_err(|e| at(e.to_string()))?;
        if d.protocol != "hidpp20" {
            return Err(at(format!("protocol {:?} is not supported; only \"hidpp20\" is (see CONTRIBUTING.md)", d.protocol)));
        }
        let per_key = match d.lighting.as_str() {
            "per-key" => true,
            "clusters" => false,
            other => return Err(at(format!("lighting {other:?} is not \"per-key\" or \"clusters\""))),
        };
        if !per_key && (!d.strip.is_empty() || !d.unknown.is_empty()) {
            return Err(at("a \"clusters\" device has only `keys` and `lights`, one per cluster".into()));
        }
        let entry = |k: KeyDef, group: &str| Zone {
            id: k.zone,
            label: k.label.unwrap_or_else(|| k.name.to_uppercase()),
            short: k.short,
            name: k.name,
            kind: Kind::Key,
            group: group.into(),
            rect: [k.at[0], k.at[1], k.size[0], k.size[1]],
        };
        let mut zones: Vec<Zone> = d.keys.into_iter().map(|k| entry(k, "keys")).collect();
        zones.extend(d.lights.into_iter().map(|k| entry(k, "lights")));
        let mut strips = vec![];
        for s in d.strip {
            if ["keys", "zones", "base"].contains(&s.name.as_str()) || !valid_name(&s.name) {
                return Err(at(format!("{:?} cannot name a strip", s.name)));
            }
            let [x, y, w, h] = s.rect;
            let n = s.zones.len().max(1) as f32;
            for (i, &zone) in s.zones.iter().enumerate() {
                zones.push(Zone {
                    id: zone,
                    name: format!("{}{}", s.name, i + 1),
                    label: (i + 1).to_string(),
                    short: None,
                    kind: Kind::Strip,
                    group: s.name.clone(),
                    rect: [x + w * i as f32 / n, y, w / n, h],
                });
            }
            strips.push(Strip { name: s.name, title: s.title, zones: s.zones });
        }
        zones.extend(d.unknown.into_iter().map(|zone| Zone {
            id: zone,
            name: String::new(),
            label: String::new(),
            short: None,
            kind: Kind::Unknown,
            group: String::new(),
            rect: [0.0; 4],
        }));
        let (mut ids, mut names) = (HashSet::new(), HashSet::new());
        for z in &zones {
            if z.id == 0xFF {
                return Err(at("zone 255 means \"skip\" on the wire and cannot be used".into()));
            }
            if !ids.insert(z.id) {
                return Err(at(format!("zone {} is listed twice", z.id)));
            }
            if z.kind != Kind::Unknown && (!valid_name(&z.name) || z.name == "base" || !names.insert(z.name.clone())) {
                return Err(at(format!("{:?} is a duplicate or invalid name (lowercase letters, digits, _)", z.name)));
            }
        }
        Ok(Model {
            id: id.into(),
            name: d.name,
            per_key,
            vendor: d.vendor,
            product: d.product,
            interface: d.interface,
            device_index: d.device_index,
            board: d.board,
            zones,
            strips,
        })
    }

    pub fn zone(&self, id: u8) -> Option<&Zone> {
        self.zones.iter().find(|z| z.id == id)
    }

    /// A key or strip segment by name; unknown zones have no name.
    pub fn named(&self, name: &str) -> Option<&Zone> {
        self.zones.iter().find(|z| !z.name.is_empty() && z.name == name)
    }

    pub fn ids(&self, kind: Kind) -> Vec<u8> {
        self.zones.iter().filter(|z| z.kind == kind).map(|z| z.id).collect()
    }

    pub fn unknown(&self) -> Vec<u8> {
        self.ids(Kind::Unknown)
    }

    /// Base-colour groups in definition order: "keys", "lights", then each strip.
    pub fn groups(&self) -> Vec<&str> {
        let mut groups: Vec<&str> = vec![];
        for z in self.zones.iter().filter(|z| z.kind != Kind::Unknown) {
            if !groups.contains(&z.group.as_str()) {
                groups.push(&z.group);
            }
        }
        groups
    }

    /// Readable reason a key name was rejected.
    pub fn why_not(&self, name: &str) -> String {
        let hint = match name {
            "shift" => "leftshift or rightshift",
            "ctrl" | "control" => "leftctrl or rightctrl",
            "alt" => "leftalt or rightalt",
            "meta" | "super" | "win" | "windows" | "gui" => "leftmeta or rightmeta",
            "escape" => "esc",
            "del" => "delete",
            "return" => "enter",
            "back" | "bksp" => "backspace",
            "caps" => "capslock",
            "num" => "numlock",
            "period" => "dot",
            _ => return format!("{name:?} is not a key on the {}; `loginux zones` lists them, and [zones] takes raw zone IDs", self.name),
        };
        format!("{name:?} is ambiguous or not a key name; use {hint}")
    }
}

/// Zone ID from "154" or "0x9a".
pub fn parse_id(text: &str) -> Option<u8> {
    match text.strip_prefix("0x") {
        Some(hex) => u8::from_str_radix(hex, 16).ok(),
        None => text.parse().ok(),
    }
}

/// Every known keyboard: the built-ins, then the user's own definitions, which
/// replace a built-in of the same ID. A broken user file is reported and skipped.
pub fn models() -> &'static [Model] {
    static MODELS: OnceLock<Vec<Model>> = OnceLock::new();
    MODELS.get_or_init(|| {
        #[cfg(test)]
        crate::testenv::init();
        let mut models: Vec<Model> =
            BUILT_IN.iter().map(|(id, text)| Model::parse(id, text).expect("built-in keyboard definition")).collect();
        let dir = crate::profile::config_dir().join("devices");
        let mut files: Vec<_> = std::fs::read_dir(&dir).into_iter().flatten().filter_map(|e| Some(e.ok()?.path())).collect();
        files.sort();
        for path in files.into_iter().filter(|p| p.extension().is_some_and(|e| e == "toml")) {
            let Some(id) = path.file_stem().and_then(|s| s.to_str()).map(String::from) else { continue };
            match std::fs::read_to_string(&path).map_err(|e| e.to_string()).and_then(|t| Model::parse(&id, &t)) {
                Ok(m) => {
                    models.retain(|old| old.id != m.id);
                    models.push(m);
                }
                Err(e) => eprintln!("loginux: {}: {e}", path.display()),
            }
        }
        models
    })
}

pub fn find(id: &str) -> Option<&'static Model> {
    models().iter().find(|m| m.id == id)
}

/// The keyboard this process works with: the one named in config.toml, else
/// the first whose USB interface is present, else the first built-in.
pub fn current() -> &'static Model {
    static CURRENT: OnceLock<&'static Model> = OnceLock::new();
    CURRENT.get_or_init(|| {
        if cfg!(test) {
            return &models()[0]; // tests rewrite config.toml as they go; all of them see the first built-in
        }
        crate::profile::chosen_keyboard()
            .and_then(|id| find(&id))
            .or_else(|| models().iter().find(|m| crate::hidpp::present(m)))
            .unwrap_or(&models()[0])
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const BASE: &str = "name = \"T\"\nprotocol = \"hidpp20\"\nvendor = 1\nproduct = 2\ninterface = 0\nboard = [4, 2]\n";

    fn def(extra: &str) -> Result<Model, String> {
        Model::parse("t", &format!("{BASE}{extra}"))
    }

    #[test]
    fn built_in_g316() {
        crate::testenv::init();
        let m = find("g316-x-98").unwrap();
        assert_eq!((m.zones.len(), m.ids(Kind::Key).len(), m.unknown().len()), (133, 95, 8));
        assert_eq!(m.named("esc").map(|z| z.id), Some(38));
        assert_eq!(m.named("bar1").map(|z| (z.id, z.group.as_str())), Some((160, "bar")));
        assert_eq!(m.named("bar30").map(|z| z.id), Some(221));
        assert_eq!(m.groups(), ["keys", "bar"]);
        assert!(m.per_key && m.device_index == 0xFF);
    }

    #[test]
    fn built_in_g903_has_only_the_logo() {
        crate::testenv::init();
        let m = find("g903-hero").unwrap();
        assert!(!m.per_key);
        assert_eq!(m.zones.iter().map(|z| (z.id, z.name.as_str(), z.group.as_str())).collect::<Vec<_>>(), [(1, "logo", "lights")]);
        assert_eq!(m.groups(), ["lights"]);
        assert_eq!(m.zone(0).map(|z| z.id), None, "the DPI indicator is left out");
    }

    #[test]
    fn user_definitions_load_from_the_config_folder() {
        crate::testenv::init();
        let pad = find("test-pad").expect("written by testenv");
        assert_eq!((pad.name.as_str(), pad.product, pad.per_key), ("Test Pad", 0xffff, false));
        assert!(find("broken").is_none(), "a broken file is skipped");
        assert!(find("notes").is_none(), "only .toml files are read");
        assert!(models().iter().any(|m| m.id == current().id));
    }

    #[test]
    fn definitions_explain_their_mistakes() {
        let key = |name: &str, zone: u8| format!("{{ name = \"{name}\", zone = {zone}, at = [0, 0] }}");
        assert!(def(&format!("keys = [{}]", key("a", 1))).unwrap().per_key);
        for (extra, says) in [
            ("keys = []\nprotocol2 = 1".to_string(), "unknown field"),
            ("lighting = \"rgb\"".to_string(), "is not \"per-key\" or \"clusters\""),
            (format!("lighting = \"clusters\"\nunknown = [3]\nlights = [{}]", key("l", 0)), "has only `keys` and `lights`"),
            (format!("keys = [{}, {}]", key("a", 1), key("b", 1)), "zone 1 is listed twice"),
            (format!("keys = [{}]", key("a", 255)), "zone 255"),
            (format!("keys = [{}]", key("Esc", 1)), "invalid name"),
            (format!("keys = [{}]", key("base", 1)), "invalid name"),
            (format!("keys = [{}, {}]", key("a", 1), key("a", 2)), "duplicate"),
            ("[[strip]]\nname = \"keys\"\ntitle = \"K\"\nrect = [0, 0, 1, 1]\nzones = [5]".to_string(), "cannot name a strip"),
        ] {
            let e = def(&extra).err().unwrap_or_default();
            assert!(e.contains(says), "{extra:?} gave {e:?}");
        }
        let other = Model::parse("x", &BASE.replace("hidpp20", "other")).err().expect("rejected");
        assert!(other.contains("not supported"));
    }

    #[test]
    fn strips_name_their_segments_in_order() {
        let m = def("unknown = [9]\n[[strip]]\nname = \"glow\"\ntitle = \"Glow\"\nrect = [0, 0, 4, 1]\nzones = [5, 6, 7]").unwrap();
        assert_eq!(m.named("glow2").map(|z| (z.id, z.kind)), Some((6, Kind::Strip)));
        assert_eq!(m.strips[0].zones, [5, 6, 7]);
        assert_eq!(m.unknown(), [9]);
        assert_eq!(m.groups(), ["glow"], "unknown zones are not a group");
    }

    #[test]
    fn names_ids_and_hints() {
        crate::testenv::init();
        let m = find("g316-x-98").unwrap();
        assert!(m.why_not("shift").contains("leftshift or rightshift"));
        assert!(m.why_not("escape").contains("use esc"));
        assert!(m.why_not("nope").contains("not a key on the Logitech G316 X 98"));
        assert_eq!((parse_id("154"), parse_id("0x9a")), (Some(154), Some(154)));
        assert_eq!((parse_id("0xzz"), parse_id("256"), parse_id("x")), (None, None, None));
        assert!(find("nope").is_none());
    }
}
