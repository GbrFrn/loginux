//! The Loginux mark, read from icons/loginux.txt (the file tools/mkicon.py also
//! draws the installed icons from) for the window and tray icons.

const SOURCE: &str = include_str!("../icons/loginux.txt");

/// The mark as RGBA with each grid pixel `scale` pixels square: (side, bytes).
pub fn rgba(scale: usize) -> (usize, Vec<u8>) {
    let mut palette = vec![('.', [0u8; 4])];
    let mut rows = vec![];
    for line in SOURCE.lines().filter(|l| !l.trim().is_empty() && !l.starts_with('#')) {
        match line.split_whitespace().collect::<Vec<_>>()[..] {
            [key, hex, ..] if key.len() == 1 && hex.starts_with('#') => {
                let v = u32::from_str_radix(&hex[1..], 16).expect("palette colour is #rrggbb");
                let [_, r, g, b] = v.to_be_bytes();
                palette.push((key.chars().next().expect("one character"), [r, g, b, 0xFF]));
            }
            _ => rows.push(line.trim().as_bytes()),
        }
    }
    let side = rows.len() * scale;
    let mut out = Vec::with_capacity(side * side * 4);
    for row in &rows {
        let line: Vec<u8> = row
            .iter()
            .flat_map(|&c| {
                let px = palette.iter().find(|p| p.0 == c as char).expect("pixel colour in the palette").1;
                std::iter::repeat_n(px, scale).flatten()
            })
            .collect();
        for _ in 0..scale {
            out.extend_from_slice(&line);
        }
    }
    (side, out)
}

#[cfg(test)]
mod tests {
    #[test]
    fn mark_is_square_and_opaque_where_drawn() {
        let (side, px) = super::rgba(2);
        assert_eq!((side, px.len()), (32, 32 * 32 * 4));
        assert_eq!(&px[..4], &[0, 0, 0, 0], "corner is transparent");
        let centre = (16 * 32 + 16) * 4;
        assert_eq!(px[centre + 3], 0xFF);
    }
}
