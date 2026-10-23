//! Built-in lighting effects: the device runs them itself once 0x8071 SetEffect
//! starts them. Parameter layouts are the 10 bytes after the effect index,
//! big-endian, as documented by Solaar's LED effect table (static evidence);
//! each device lists which effect IDs its clusters accept.

use crate::profile::{Rgb, hex, parse_rgb};
use serde::{Deserialize, Serialize};

/// One effect with its parameters, ready to encode.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Effect {
    pub kind: Kind,
    pub color: Rgb,
    /// Milliseconds per cycle; for Ripple, Solaar's 2-200 range.
    pub period: u16,
    /// 0-100.
    pub intensity: u8,
    /// 0-255.
    pub saturation: u8,
    /// Wave direction, Solaar's numbering: 1 right, 2 down, 6 left, 7 up, 3 centre out, 8 centre in.
    pub direction: u8,
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    Static,
    Cycle,
    Breathe,
    Wave,
    Ripple,
}

/// Wave directions by name, in Solaar's numbering.
pub const DIRECTIONS: [(&str, u8); 6] =
    [("right", 1), ("left", 6), ("down", 2), ("up", 7), ("centre-out", 3), ("centre-in", 8)];

impl Kind {
    pub const ALL: [Kind; 5] = [Kind::Static, Kind::Breathe, Kind::Cycle, Kind::Wave, Kind::Ripple];

    pub fn name(self) -> &'static str {
        match self {
            Kind::Static => "static",
            Kind::Cycle => "cycle",
            Kind::Breathe => "breathe",
            Kind::Wave => "wave",
            Kind::Ripple => "ripple",
        }
    }

    pub fn from_name(name: &str) -> Result<Kind, String> {
        Kind::ALL
            .into_iter()
            .find(|k| k.name() == name)
            .ok_or_else(|| format!("{name:?} is not an effect: static, breathe, cycle, wave or ripple"))
    }

    /// Effect IDs that implement this kind, preferred first.
    fn ids(self) -> &'static [u16] {
        match self {
            Kind::Static => &[0x01],
            Kind::Cycle => &[0x15, 0x03],
            Kind::Breathe => &[0x0A],
            Kind::Wave => &[0x16, 0x04],
            Kind::Ripple => &[0x17, 0x0B],
        }
    }

    /// Whether a cluster listing `available` can run this kind.
    pub fn on(self, available: &[u16]) -> bool {
        self.ids().iter().any(|id| available.contains(id))
    }

    /// Whether the colour parameter means anything for this kind.
    pub fn coloured(self) -> bool {
        matches!(self, Kind::Static | Kind::Breathe | Kind::Ripple)
    }
}

impl Effect {
    /// The kind with its default parameters.
    pub fn new(kind: Kind) -> Effect {
        Effect {
            kind,
            color: [0xFF; 3],
            period: if kind == Kind::Ripple { 20 } else { 5000 },
            intensity: 100,
            saturation: 255,
            direction: 1,
        }
    }

    /// "cycle", "wave,period=3000,direction=left", "breathe,color=ff0000"...
    pub fn parse(text: &str) -> Result<Effect, String> {
        let mut parts = text.split(',');
        let mut e = Effect::new(Kind::from_name(parts.next().unwrap_or_default())?);
        for part in parts {
            let (key, value) = part.split_once('=').ok_or_else(|| format!("{part:?} is not key=value"))?;
            e.set(key, value)?;
        }
        Ok(e)
    }

    /// The `[effect]` table of a profile: `kind` and the same keys as `parse`.
    pub fn from_table(table: &toml::Table) -> Result<Effect, String> {
        let text = |v: &toml::Value| match v {
            toml::Value::String(s) => Ok(s.clone()),
            toml::Value::Integer(n) => Ok(n.to_string()),
            other => Err(format!("{other} is neither text nor a whole number")),
        };
        let kind = table.get("kind").ok_or("[effect] needs a kind: breathe, cycle, wave, ripple or static")?;
        let mut e = Effect::new(Kind::from_name(&text(kind)?)?);
        for (key, value) in table.iter().filter(|(k, _)| *k != "kind") {
            e.set(key, &text(value).map_err(|err| format!("[effect] {key}: {err}"))?)
                .map_err(|err| format!("[effect] {err}"))?;
        }
        Ok(e)
    }

    /// The `[effect]` table, with only the keys that matter for the kind.
    pub fn to_table(&self) -> String {
        let mut out = format!("[effect]\nkind = \"{}\"\n", self.kind.name());
        if self.kind.coloured() {
            out += &format!("color = \"{}\"\n", hex(self.color));
        }
        if self.kind != Kind::Static {
            out += &format!("period = {}\n", self.period);
        }
        if self.kind == Kind::Wave {
            out += &format!("direction = \"{}\"\n", self.direction_name());
        }
        out
    }

    pub fn direction_name(&self) -> &'static str {
        DIRECTIONS.iter().find(|d| d.1 == self.direction).map_or("right", |d| d.0)
    }

    fn set(&mut self, key: &str, value: &str) -> Result<(), String> {
        let num = |max: u32| value.parse::<u32>().ok().filter(|v| *v <= max).ok_or_else(|| format!("{key}={value} is out of range"));
        match key {
            "color" => self.color = parse_rgb(value)?,
            "period" => self.period = num(u16::MAX.into())? as u16,
            "intensity" => self.intensity = num(100)? as u8,
            "saturation" => self.saturation = num(255)? as u8,
            "direction" => {
                self.direction = match DIRECTIONS.iter().find(|d| d.0 == value) {
                    Some(d) => d.1,
                    None => num(8)? as u8,
                }
            }
            _ => return Err(format!("{key:?} is not an effect parameter")),
        }
        Ok(())
    }

    /// The effect ID to use among those a cluster lists, and its 10 parameter bytes.
    pub fn encode(&self, available: &[u16]) -> Result<(u16, [u8; 10]), String> {
        let id = *self
            .kind
            .ids()
            .iter()
            .find(|id| available.contains(id))
            .ok_or_else(|| format!("this cluster has no {:?} effect (it lists {available:02x?})", self.kind))?;
        let mut p = [0u8; 10];
        let [r, g, b] = self.color;
        let period = self.period.to_be_bytes();
        match id {
            0x01 => p[..3].copy_from_slice(&[r, g, b]),
            0x03 => {
                p[5..7].copy_from_slice(&period);
                p[7] = self.intensity;
            }
            0x0A => {
                p[..3].copy_from_slice(&[r, g, b]);
                p[3..5].copy_from_slice(&period);
                p[6] = self.intensity;
            }
            0x15 => {
                p[1] = self.saturation;
                p[6..8].copy_from_slice(&period);
                p[8] = self.intensity;
            }
            0x04 | 0x16 => {
                p[1] = self.saturation;
                p[6..8].copy_from_slice(&period);
                p[8] = self.intensity;
                p[9] = self.direction;
            }
            0x0B => {
                p[..3].copy_from_slice(&[r, g, b]);
                p[4..6].copy_from_slice(&period);
            }
            _ => {
                // 0x17: the one ID Kind::ids lists that the arms above leave
                p[..3].copy_from_slice(&[r, g, b]);
                p[3] = self.saturation;
                p[6..8].copy_from_slice(&period);
            }
        }
        Ok((id, p))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layouts_follow_solaar() {
        let wave = Effect::parse("wave,period=3000,direction=left").unwrap();
        assert_eq!(wave.encode(&[0x01, 0x16]).unwrap(), (0x16, [0, 255, 0, 0, 0, 0, 0x0b, 0xb8, 100, 6]));
        let cycle = Effect::parse("cycle").unwrap();
        assert_eq!(cycle.encode(&[0x03]).unwrap(), (0x03, [0, 0, 0, 0, 0, 0x13, 0x88, 100, 0, 0]));
        assert!(Effect::parse("ripple").unwrap().encode(&[0x01, 0x03]).is_err());
        assert!(Effect::parse("glow").is_err());
    }

    #[test]
    fn every_layout_puts_the_parameters_where_solaar_says() {
        let e = Effect { kind: Kind::Static, color: [1, 2, 3], period: 0x1234, intensity: 50, saturation: 200, direction: 6 };
        let with = |kind| Effect { kind, ..e.clone() };
        assert_eq!(e.encode(&[0x01]).unwrap(), (0x01, [1, 2, 3, 0, 0, 0, 0, 0, 0, 0]));
        assert_eq!(with(Kind::Cycle).encode(&[0x03]).unwrap().1, [0, 0, 0, 0, 0, 0x12, 0x34, 50, 0, 0]);
        assert_eq!(with(Kind::Cycle).encode(&[0x15]).unwrap().1, [0, 200, 0, 0, 0, 0, 0x12, 0x34, 50, 0]);
        assert_eq!(with(Kind::Cycle).encode(&[0x03, 0x15]).unwrap().0, 0x15, "the newer cycle wins");
        assert_eq!(with(Kind::Breathe).encode(&[0x0a]).unwrap().1, [1, 2, 3, 0x12, 0x34, 0, 50, 0, 0, 0]);
        assert_eq!(with(Kind::Wave).encode(&[0x04]).unwrap().1, [0, 200, 0, 0, 0, 0, 0x12, 0x34, 50, 6]);
        assert_eq!(with(Kind::Wave).encode(&[0x16]).unwrap().1, [0, 200, 0, 0, 0, 0, 0x12, 0x34, 50, 6]);
        assert_eq!(with(Kind::Ripple).encode(&[0x0b]).unwrap().1, [1, 2, 3, 0, 0x12, 0x34, 0, 0, 0, 0]);
        assert_eq!(with(Kind::Ripple).encode(&[0x17]).unwrap().1, [1, 2, 3, 200, 0, 0, 0x12, 0x34, 0, 0]);
        assert!(with(Kind::Breathe).encode(&[0x01, 0x03]).unwrap_err().contains("no Breathe effect"));
    }

    #[test]
    fn kinds_by_name_and_capability() {
        for k in Kind::ALL {
            assert_eq!(Kind::from_name(k.name()).unwrap(), k);
        }
        assert!(Kind::from_name("sparkle").is_err());
        assert!(Kind::Wave.on(&[0x01, 0x04]) && !Kind::Wave.on(&[0x01, 0x0a]));
        assert!(Kind::Breathe.coloured() && Kind::Ripple.coloured() && Kind::Static.coloured());
        assert!(!Kind::Cycle.coloured() && !Kind::Wave.coloured());
        assert_eq!((Effect::new(Kind::Ripple).period, Effect::new(Kind::Wave).period), (20, 5000));
    }

    #[test]
    fn parsing_effect_arguments() {
        assert_eq!(Effect::parse("wave,direction=centre-in").unwrap().direction, 8);
        assert_eq!(Effect::parse("wave,direction=7").unwrap().direction, 7);
        let b = Effect::parse("breathe,color=00ff00,intensity=40,saturation=10").unwrap();
        assert_eq!((b.color, b.intensity, b.saturation), ([0, 255, 0], 40, 10));
        for (text, says) in [
            ("wave,period", "not key=value"),
            ("breathe,intensity=101", "out of range"),
            ("wave,direction=9", "out of range"),
            ("cycle,saturation=256", "out of range"),
            ("cycle,speed=3", "not an effect parameter"),
            ("breathe,color=red", "not a colour"),
        ] {
            assert!(Effect::parse(text).unwrap_err().contains(says), "{text}");
        }
    }

    #[test]
    fn tables_write_only_what_the_kind_uses() {
        assert_eq!(Effect::new(Kind::Static).to_table(), "[effect]\nkind = \"static\"\ncolor = \"#ffffff\"\n");
        assert_eq!(Effect::new(Kind::Cycle).to_table(), "[effect]\nkind = \"cycle\"\nperiod = 5000\n");
        let wave = Effect::parse("wave,direction=down").unwrap().to_table();
        assert!(wave.ends_with("direction = \"down\"\n") && !wave.contains("color"), "{wave}");
        let odd = Effect { direction: 4, ..Effect::new(Kind::Wave) };
        assert_eq!(odd.direction_name(), "right", "an unnamed direction prints as the default");
    }

    #[test]
    fn table_round_trips() {
        let e = Effect::parse("wave,period=2500,direction=up").unwrap();
        let doc: toml::Table = e.to_table().parse().unwrap();
        assert_eq!(Effect::from_table(doc["effect"].as_table().unwrap()).unwrap(), e);
    }
}
