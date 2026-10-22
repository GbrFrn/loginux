//! Lighting profiles, the chosen profile, and channel-check observations.
//! Everything lives under $XDG_CONFIG_HOME/loginux (default ~/.config/loginux):
//!
//!   profiles/NAME.toml   one profile per file
//!   config.toml          the profile loginux keeps applied, and the keyboard last seen
//!   checks/ID.toml       what lit up during channel checks, per keyboard model
//!   devices/ID.toml      the user's own device definitions (see layout.rs)

use crate::effects::Effect;
use crate::layout::{self, Kind, Model};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

pub type Rgb = [u8; 3];

pub fn parse_rgb(text: &str) -> Result<Rgb, String> {
    let hex = text.strip_prefix('#').unwrap_or(text);
    if hex.len() != 6 || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(format!("{text:?} is not a colour; use #rrggbb"));
    }
    let v = u32::from_str_radix(hex, 16).map_err(|e| e.to_string())?;
    Ok([(v >> 16) as u8, (v >> 8) as u8, v as u8])
}

pub fn hex([r, g, b]: Rgb) -> String {
    format!("#{r:02x}{g:02x}{b:02x}")
}

/// A colour for every key, light and strip segment. `bases` holds the colour of
/// each group ("keys", "lights", and each strip by name, e.g. "bar"); `zones`
/// overrides single zones. Unknown zones are lit only when `zones` names them.
/// With `effect`, the device runs that built-in effect instead of the colours.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Profile {
    pub bases: BTreeMap<String, Rgb>,
    pub zones: BTreeMap<u8, Rgb>,
    #[serde(default)]
    pub effect: Option<Effect>,
}

/// Built-in presets, effects the device runs itself. A profile file of the same
/// name replaces one.
const PRESETS: &[(&str, &str)] = &[
    ("Breathing", "[effect]\nkind = \"breathe\"\ncolor = \"#ff2e3f\"\nperiod = 4000\n"),
    ("Colour Cycle", "[effect]\nkind = \"cycle\"\nperiod = 10000\n"),
    ("Wave", "[effect]\nkind = \"wave\"\nperiod = 3000\ndirection = \"right\"\n"),
    ("Ripple", "[effect]\nkind = \"ripple\"\ncolor = \"#ff2e3f\"\nperiod = 20\n"),
];

impl Profile {
    /// Parses a profile for `model`: one table per group ([keys], [lights], each
    /// strip), [zones] and [effect].
    pub fn parse(model: &Model, text: &str) -> Result<Self, String> {
        let doc: toml::Table = text.parse().map_err(|e: toml::de::Error| e.to_string())?;
        let mut p = Profile::default();
        let mut by = BTreeMap::<u8, String>::new();
        let groups = model.groups();
        for (section, value) in &doc {
            let table = value.as_table().ok_or_else(|| format!("{section} must be a [{section}] table"))?;
            if section == "effect" {
                p.effect = Some(Effect::from_table(table)?);
                continue;
            }
            if section != "zones" && !groups.contains(&section.as_str()) {
                let tables: Vec<String> = groups.iter().map(|g| format!("[{g}]")).collect();
                return Err(format!("unknown section [{section}]; the {} takes {}, [zones] and [effect]", model.name, tables.join(", ")));
            }
            for (key, value) in table {
                let at = format!("[{section}] {key}");
                let text = value.as_str().ok_or_else(|| format!("{at}: write the colour as a string, e.g. \"#ff0000\""))?;
                let colour = parse_rgb(text).map_err(|e| format!("{at}: {e}"))?;
                let name = key.to_lowercase();
                let id = if section == "zones" {
                    match layout::parse_id(&name).and_then(|id| model.zone(id)) {
                        Some(z) => z.id,
                        None => return Err(format!("{at}: not one of the {}'s zone IDs", model.name)),
                    }
                } else if name == "base" {
                    p.bases.insert(section.clone(), colour);
                    continue;
                } else {
                    match model.named(&name) {
                        Some(z) if z.group == *section => z.id,
                        _ if section == "keys" => return Err(format!("{at}: {}", model.why_not(&name))),
                        _ => return Err(format!("{at}: [{section}] takes base and {section}1 to {section}N")),
                    }
                };
                if let Some(first) = by.insert(id, at.clone()) {
                    return Err(format!("zone {id} is set twice: {first} and {at}"));
                }
                p.zones.insert(id, colour);
            }
        }
        Ok(p)
    }

    pub fn to_toml(&self, model: &Model) -> String {
        let mut out = String::from(
            "# Loginux lighting profile. Colours are #rrggbb.\n\
             # base fills every key, light or strip segment of its table that is not listed.\n\
             # [zones] takes raw zone IDs; it is the only way to light the unknown zones.\n\
             # [effect], when present, has the device run one of its own effects instead.\n",
        );
        if let Some(e) = &self.effect {
            out += &format!("\n{}", e.to_table());
        }
        for group in model.groups() {
            out += &format!("\n[{group}]\nbase = \"{}\"\n", hex(self.base(group)));
            for z in model.zones.iter().filter(|z| z.group == group) {
                if let Some(c) = self.zones.get(&z.id) {
                    out += &format!("{} = \"{}\"\n", z.name, hex(*c));
                }
            }
        }
        let raw: Vec<String> = model
            .zones
            .iter()
            .filter(|z| z.kind == Kind::Unknown)
            .filter_map(|z| Some(format!("{} = \"{}\"\n", z.id, hex(*self.zones.get(&z.id)?))))
            .collect();
        if !raw.is_empty() {
            out += &format!("\n[zones]\n{}", raw.concat());
        }
        out
    }

    /// Base colour of a group; groups a profile does not mention are dark.
    pub fn base(&self, group: &str) -> Rgb {
        self.bases.get(group).copied().unwrap_or_default()
    }

    /// The colour loginux sends to `id`, if any.
    pub fn colour(&self, model: &Model, id: u8) -> Option<Rgb> {
        let z = model.zone(id)?;
        self.zones.get(&id).copied().or((z.kind != Kind::Unknown).then(|| self.base(&z.group)))
    }

    /// (zone, colour) for every zone the keyboard reports that this profile lights.
    /// Fails before anything is sent when an explicit zone is not on the keyboard.
    pub fn frame(&self, model: &Model, supported: &[u8]) -> Result<Vec<(u8, Rgb)>, String> {
        if let Some(id) = self.zones.keys().find(|id| !supported.contains(id)) {
            return Err(format!("the profile sets zone {id}, which this keyboard does not report"));
        }
        Ok(supported.iter().filter_map(|&id| Some((id, self.colour(model, id)?))).collect())
    }

    /// Every zone is in the model; enforced on profiles that arrive over the socket.
    pub fn check_zones(&self, model: &Model) -> Result<(), String> {
        match self.zones.keys().find(|&&id| model.zone(id).is_none()) {
            Some(id) => Err(format!("zone {id} is not one of the {}'s zones", model.name)),
            None => Ok(()),
        }
    }
}

pub fn config_dir() -> PathBuf {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .unwrap_or_else(|| PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".config"));
    base.join("loginux")
}

pub fn profiles_dir() -> PathBuf {
    config_dir().join("profiles")
}

/// Letters, digits, spaces, '-' and '_'; no path separators or dots.
pub fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && name.trim() == name
        && name.chars().all(|c| c.is_ascii_alphanumeric() || " -_".contains(c))
}

pub fn path(name: &str) -> Result<PathBuf, String> {
    if !valid_name(name) {
        return Err(format!("{name:?} is not a profile name (letters, digits, spaces, - and _)"));
    }
    Ok(profiles_dir().join(format!("{name}.toml")))
}

pub fn read(model: &Model, path: &Path) -> Result<Profile, String> {
    let text = fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
    Profile::parse(model, &text).map_err(|e| format!("{}: {e}", path.display()))
}

/// A profile by name: its file, or the built-in preset of that name.
pub fn load(model: &Model, name: &str) -> Result<Profile, String> {
    let file = path(name)?;
    match PRESETS.iter().find(|p| p.0 == name) {
        Some((_, text)) if !file.exists() => Profile::parse(model, text),
        _ => read(model, &file),
    }
}

/// A built-in preset that no profile file replaces.
pub fn built_in(name: &str) -> bool {
    PRESETS.iter().any(|p| p.0 == name) && path(name).is_ok_and(|f| !f.exists())
}

/// A profile by name, or from a file when the argument looks like a path.
pub fn load_arg(model: &Model, arg: &str) -> Result<Profile, String> {
    if arg.contains('/') || arg.ends_with(".toml") { read(model, Path::new(arg)) } else { load(model, arg) }
}

pub fn save(model: &Model, name: &str, profile: &Profile) -> Result<(), String> {
    write_atomic(&path(name)?, &profile.to_toml(model))
}

pub fn delete(name: &str) -> Result<(), String> {
    if built_in(name) {
        return Err(format!("{name} is built in"));
    }
    fs::remove_file(path(name)?).map_err(|e| e.to_string())
}

pub fn list() -> Vec<String> {
    let mut names: Vec<String> = fs::read_dir(profiles_dir())
        .into_iter()
        .flatten()
        .filter_map(|e| e.ok()?.file_name().into_string().ok()?.strip_suffix(".toml").map(String::from))
        .filter(|n| valid_name(n))
        .collect();
    names.sort_by_key(|n| n.to_lowercase());
    let presets: Vec<String> = PRESETS.iter().map(|p| p.0.to_string()).filter(|n| !names.contains(n)).collect();
    names.extend(presets);
    names
}

pub fn modified(path: &Path) -> Option<SystemTime> {
    fs::metadata(path).and_then(|m| m.modified()).ok()
}

fn config_value(key: &str) -> Option<String> {
    let text = fs::read_to_string(config_dir().join("config.toml")).ok()?;
    let doc: toml::Table = text.parse().ok()?;
    doc.get(key)?.as_str().map(String::from)
}

/// The profile the engine keeps applied, from config.toml.
pub fn active() -> Option<String> {
    config_value("profile")
}

/// The keyboard definition last connected, so the editor can show it unplugged.
pub fn chosen_keyboard() -> Option<String> {
    config_value("keyboard")
}

fn write_config(profile: Option<&str>, keyboard: Option<&str>) -> Result<(), String> {
    let mut text = String::from("# Written by Loginux: the profile it keeps applied and the keyboard last seen.\n");
    if let Some(name) = profile {
        path(name)?;
        text += &format!("profile = \"{name}\"\n");
    }
    if let Some(id) = keyboard.filter(|id| id.chars().all(|c| c.is_ascii_alphanumeric() || "-_".contains(c))) {
        text += &format!("keyboard = \"{id}\"\n");
    }
    write_atomic(&config_dir().join("config.toml"), &text)
}

pub fn set_active(name: Option<&str>) -> Result<(), String> {
    write_config(name, chosen_keyboard().as_deref())
}

pub fn set_keyboard(id: &str) -> Result<(), String> {
    write_config(active().as_deref(), Some(id))
}

/// What lit up when loginux lit one zone during a channel check.
#[derive(Clone, Debug, PartialEq)]
pub struct Seen {
    /// A key or strip-segment name, "nothing", or "elsewhere".
    pub saw: String,
    pub date: String,
}

/// Answers are per keyboard model: a zone number means a different LED on another board.
pub fn checks_path(model: &Model) -> PathBuf {
    config_dir().join("checks").join(format!("{}.toml", model.id))
}

pub fn load_checks(model: &Model) -> BTreeMap<u8, Seen> {
    let doc: toml::Table = fs::read_to_string(checks_path(model)).ok().and_then(|t| t.parse().ok()).unwrap_or_default();
    let Some(zones) = doc.get("zones").and_then(|z| z.as_table()) else { return BTreeMap::new() };
    zones
        .iter()
        .filter_map(|(k, v)| {
            let field = |f: &str| v.get(f)?.as_str().map(String::from);
            Some((layout::parse_id(k)?, Seen { saw: field("saw")?, date: field("date").unwrap_or_default() }))
        })
        .collect()
}

pub fn save_checks(model: &Model, checks: &BTreeMap<u8, Seen>) -> Result<(), String> {
    let mut text = format!(
        "# Loginux channel check on a {}: what lit up when Loginux lit one zone at a time.\n\
         # saw = the key or strip segment that lit, \"nothing\", or \"elsewhere\".\n\n[zones]\n",
        model.name
    );
    for (id, seen) in checks {
        text += &format!("{id} = {{ saw = \"{}\", date = \"{}\" }}\n", seen.saw, seen.date);
    }
    write_atomic(&checks_path(model), &text)
}

/// Today's date in UTC, YYYY-MM-DD.
pub fn today() -> String {
    let days = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs() / 86_400) as i64;
    // Howard Hinnant's civil_from_days.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!("{y:04}-{m:02}-{d:02}")
}

fn write_atomic(path: &Path, text: &str) -> Result<(), String> {
    let dir = path.parent().ok_or("no parent directory")?;
    fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let tmp = path.with_extension("toml.tmp");
    fs::write(&tmp, text).and_then(|_| fs::rename(&tmp, path)).map_err(|e| format!("{}: {e}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_and_rejections() {
        let m = &layout::models()[0];
        let p = Profile::parse(m, "[keys]\nbase = \"#102030\"\nEsc = \"ff0000\"\n[bar]\nbar1 = \"#00ff00\"\n[zones]\n154 = \"#ffffff\"\n")
            .unwrap();
        assert_eq!(p.base("keys"), [0x10, 0x20, 0x30]);
        assert_eq!(p.zones[&38], [0xff, 0, 0]);
        assert_eq!(p.zones[&160], [0, 0xff, 0]);
        assert_eq!(Profile::parse(m, &p.to_toml(m)).unwrap(), p);
        assert_eq!(p.colour(m, 47), None); // unknown zones stay dark unless listed
        assert!(Profile::parse(m, "[keys]\nshift = \"#ffffff\"").unwrap_err().contains("leftshift"));
        assert!(Profile::parse(m, "[keys]\nesc = \"#fff\"").is_err());
        assert!(Profile::parse(m, "[keys]\nesc = \"#ffffff\"\n[zones]\n38 = \"#000000\"").unwrap_err().contains("twice"));
        assert!(Profile::parse(m, "[lights]\nbase = \"#ffffff\"").is_err());
        assert!(p.frame(m, &[38, 160]).is_err()); // 154 is set but not reported
        assert_eq!(today().len(), 10);
        assert!(!valid_name("../x") && !valid_name(".hidden") && valid_name("Arrow keys"));
    }
}
