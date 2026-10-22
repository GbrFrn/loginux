//! Built-in lighting effects: the device runs them itself once 0x8071 SetEffect
//! starts them. Parameter layouts are the 10 bytes after the effect index,
//! big-endian, as documented by Solaar's LED effect table (static evidence);
//! each device lists which effect IDs its clusters accept.

use crate::profile::{Rgb, parse_rgb};

/// One effect with its parameters, ready to encode.
#[derive(Clone, Debug, PartialEq)]
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

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Kind {
    Static,
    Cycle,
    Breathe,
    Wave,
    Ripple,
}

impl Kind {
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
}

impl Effect {
    /// "cycle", "wave,period=3000,direction=6", "breathe,color=ff0000"...
    pub fn parse(text: &str) -> Result<Effect, String> {
        let mut parts = text.split(',');
        let kind = match parts.next().unwrap_or_default() {
            "static" => Kind::Static,
            "cycle" => Kind::Cycle,
            "breathe" => Kind::Breathe,
            "wave" => Kind::Wave,
            "ripple" => Kind::Ripple,
            other => return Err(format!("{other:?} is not an effect: static, cycle, breathe, wave or ripple")),
        };
        let mut e = Effect {
            kind,
            color: [0xFF; 3],
            period: if kind == Kind::Ripple { 20 } else { 5000 },
            intensity: 100,
            saturation: 255,
            direction: 1,
        };
        for part in parts {
            let (key, value) = part.split_once('=').ok_or_else(|| format!("{part:?} is not key=value"))?;
            let num = |max: u32| value.parse::<u32>().ok().filter(|v| *v <= max).ok_or_else(|| format!("{key}={value} is out of range"));
            match key {
                "color" => e.color = parse_rgb(value)?,
                "period" => e.period = num(u16::MAX.into())? as u16,
                "intensity" => e.intensity = num(100)? as u8,
                "saturation" => e.saturation = num(255)? as u8,
                "direction" => e.direction = num(8)? as u8,
                _ => return Err(format!("{key:?} is not an effect parameter")),
            }
        }
        Ok(e)
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
            0x17 => {
                p[..3].copy_from_slice(&[r, g, b]);
                p[3] = self.saturation;
                p[6..8].copy_from_slice(&period);
            }
            _ => unreachable!("every listed ID has a layout"),
        }
        Ok((id, p))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layouts_follow_solaar() {
        let wave = Effect::parse("wave,period=3000,direction=6").unwrap();
        assert_eq!(wave.encode(&[0x01, 0x16]).unwrap(), (0x16, [0, 255, 0, 0, 0, 0, 0x0b, 0xb8, 100, 6]));
        let cycle = Effect::parse("cycle").unwrap();
        assert_eq!(cycle.encode(&[0x03]).unwrap(), (0x03, [0, 0, 0, 0, 0, 0x13, 0x88, 100, 0, 0]));
        assert!(Effect::parse("ripple").unwrap().encode(&[0x01, 0x03]).is_err());
        assert!(Effect::parse("glow").is_err());
    }
}
