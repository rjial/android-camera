//! TV-style "NO SIGNAL" picture (colour bars with a caption) shown on the outputs while there is no video.
//! The OBS plugin draws the same picture in C (obs-plugin/src/acam-source.c); keep the two in step.

use rust_i18n::t;

/// Why there is no video; sent to the OBS plugin in the idle header (PROTOCOL.md, "Berbagi ke OBS").
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reason {
    /// No session with the phone (unplugged, app closed, reconnecting).
    Disconnected = 0,
    /// Session is up but frames stopped arriving.
    Lost = 1,
    /// Sharing to OBS was switched off in acam.
    SharingOff = 2,
}

impl Reason {
    pub fn caption(self) -> String {
        match self {
            Reason::Disconnected => t!("nosignal.disconnected"),
            Reason::Lost => t!("nosignal.lost"),
            Reason::SharingOff => t!("nosignal.sharing_off"),
        }
        .to_string()
    }
}

/// 5x7 glyphs, one byte per row, bit 4 = leftmost column. Anything else renders as a space.
fn glyph(c: char) -> [u8; 7] {
    match c {
        'A' => [0x0E, 0x11, 0x11, 0x1F, 0x11, 0x11, 0x11],
        'B' => [0x1E, 0x11, 0x11, 0x1E, 0x11, 0x11, 0x1E],
        'C' => [0x0E, 0x11, 0x10, 0x10, 0x10, 0x11, 0x0E],
        'D' => [0x1E, 0x11, 0x11, 0x11, 0x11, 0x11, 0x1E],
        'E' => [0x1F, 0x10, 0x10, 0x1E, 0x10, 0x10, 0x1F],
        'F' => [0x1F, 0x10, 0x10, 0x1E, 0x10, 0x10, 0x10],
        'G' => [0x0E, 0x11, 0x10, 0x17, 0x11, 0x11, 0x0F],
        'H' => [0x11, 0x11, 0x11, 0x1F, 0x11, 0x11, 0x11],
        'I' => [0x0E, 0x04, 0x04, 0x04, 0x04, 0x04, 0x0E],
        'J' => [0x07, 0x02, 0x02, 0x02, 0x02, 0x12, 0x0C],
        'K' => [0x11, 0x12, 0x14, 0x18, 0x14, 0x12, 0x11],
        'L' => [0x10, 0x10, 0x10, 0x10, 0x10, 0x10, 0x1F],
        'M' => [0x11, 0x1B, 0x15, 0x15, 0x11, 0x11, 0x11],
        'N' => [0x11, 0x11, 0x19, 0x15, 0x13, 0x11, 0x11],
        'O' => [0x0E, 0x11, 0x11, 0x11, 0x11, 0x11, 0x0E],
        'P' => [0x1E, 0x11, 0x11, 0x1E, 0x10, 0x10, 0x10],
        'Q' => [0x0E, 0x11, 0x11, 0x11, 0x15, 0x12, 0x0D],
        'R' => [0x1E, 0x11, 0x11, 0x1E, 0x14, 0x12, 0x11],
        'S' => [0x0F, 0x10, 0x10, 0x0E, 0x01, 0x01, 0x1E],
        'T' => [0x1F, 0x04, 0x04, 0x04, 0x04, 0x04, 0x04],
        'U' => [0x11, 0x11, 0x11, 0x11, 0x11, 0x11, 0x0E],
        'V' => [0x11, 0x11, 0x11, 0x11, 0x11, 0x0A, 0x04],
        'W' => [0x11, 0x11, 0x11, 0x15, 0x15, 0x15, 0x0A],
        'X' => [0x11, 0x11, 0x0A, 0x04, 0x0A, 0x11, 0x11],
        'Y' => [0x11, 0x11, 0x11, 0x0A, 0x04, 0x04, 0x04],
        'Z' => [0x1F, 0x01, 0x02, 0x04, 0x08, 0x10, 0x1F],
        _ => [0; 7],
    }
}

/// Uppercases and strips Latin accents so a translation fits the A-Z font; other characters show as spaces.
fn fold(s: &str) -> String {
    s.to_uppercase()
        .chars()
        .map(|c| match c {
            'À'..='Å' => 'A',
            'Ç' => 'C',
            'È'..='Ë' => 'E',
            'Ì'..='Ï' => 'I',
            'Ñ' => 'N',
            'Ò'..='Ö' | 'Ø' => 'O',
            'Ù'..='Ü' => 'U',
            'Ý' | 'Ÿ' => 'Y',
            c => c,
        })
        .collect()
}

struct Text<'a> {
    s: &'a str,
    x: usize,
    y: usize,
    scale: usize,
}

impl Text<'_> {
    fn width(s: &str, scale: usize) -> usize {
        (s.chars().count() * 6).saturating_sub(1) * scale
    }

    fn hit(&self, x: usize, y: usize) -> bool {
        let cell = 6 * self.scale;
        if x < self.x || y < self.y || y >= self.y + 7 * self.scale || x >= self.x + Text::width(self.s, self.scale) {
            return false;
        }
        let (dx, row) = (x - self.x, (y - self.y) / self.scale);
        let col = (dx % cell) / self.scale;
        col < 5 && self.s.chars().nth(dx / cell).is_some_and(|c| glyph(c)[row] >> (4 - col) & 1 == 1)
    }
}

/// SMPTE-style bars at 75%: seven bars, a castellation strip, then -I / white / +Q / black.
fn bars(x: usize, y: usize, w: usize, h: usize) -> [u8; 3] {
    const TOP: [[u8; 3]; 7] = [
        [191, 191, 191],
        [191, 191, 0],
        [0, 191, 191],
        [0, 191, 0],
        [191, 0, 191],
        [191, 0, 0],
        [0, 0, 191],
    ];
    const MID: [[u8; 3]; 7] =
        [[0, 0, 191], [19, 19, 19], [191, 0, 191], [19, 19, 19], [0, 191, 191], [19, 19, 19], [191, 191, 191]];
    let bar = (x * 7 / w).min(6);
    if y < h * 2 / 3 {
        TOP[bar]
    } else if y < h * 3 / 4 {
        MID[bar]
    } else {
        match x * 28 / w {
            0..=4 => [0, 33, 76],
            5..=9 => [255, 255, 255],
            10..=14 => [50, 0, 106],
            _ => [19, 19, 19],
        }
    }
}

/// Packed I420 (limited range; BT.709 from 720 lines up, else BT.601, matching the shared frames).
pub fn render(w: usize, h: usize, caption: &str) -> Vec<u8> {
    let (w, h) = (w & !1, h & !1);
    let (title, caption) = (fold(&t!("nosignal.title")), fold(caption));
    let (title, caption) = (title.as_str(), caption.as_str());
    let ts = (h / 90).max(1);
    let cs = (h / 180).max(1);
    let pad = 4 * ts;
    let tw = Text::width(title, ts);
    let cw = Text::width(caption, cs);
    let bw = (tw.max(cw) + 2 * pad).min(w);
    let bh = (7 * ts + 7 * cs + 3 * pad).min(h);
    let (bx, by) = ((w - bw) / 2, (h - bh) / 2);
    let texts = [
        Text { s: title, x: (w.saturating_sub(tw)) / 2, y: by + pad, scale: ts },
        Text { s: caption, x: (w.saturating_sub(cw)) / 2, y: by + 2 * pad + 7 * ts, scale: cs },
    ];
    let rgb = |x: usize, y: usize| -> [u8; 3] {
        if x >= bx && x < bx + bw && y >= by && y < by + bh {
            if texts.iter().any(|t| t.hit(x, y)) { [235, 235, 235] } else { [16, 16, 16] }
        } else {
            bars(x, y, w, h)
        }
    };
    let (kr, kb) = if h >= 720 { (0.2126, 0.0722) } else { (0.299, 0.114) };
    let yuv = |[r, g, b]: [u8; 3]| -> (u8, u8, u8) {
        let (r, g, b) = (r as f32 / 255.0, g as f32 / 255.0, b as f32 / 255.0);
        let y = kr * r + (1.0 - kr - kb) * g + kb * b;
        let u = (b - y) / (2.0 * (1.0 - kb));
        let v = (r - y) / (2.0 * (1.0 - kr));
        ((16.0 + 219.0 * y).round() as u8, (128.0 + 224.0 * u).round() as u8, (128.0 + 224.0 * v).round() as u8)
    };
    let mut out = vec![0u8; w * h * 3 / 2];
    let (yp, uv) = out.split_at_mut(w * h);
    let (up, vp) = uv.split_at_mut(w * h / 4);
    for y in 0..h {
        for x in 0..w {
            let (ly, u, v) = yuv(rgb(x, y));
            yp[y * w + x] = ly;
            if x % 2 == 0 && y % 2 == 0 {
                up[(y / 2) * (w / 2) + x / 2] = u;
                vp[(y / 2) * (w / 2) + x / 2] = v;
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn draws_bars_and_caption_box() {
        let (w, h) = (1280, 720);
        let f = render(w, h, &Reason::Lost.caption());
        assert_eq!(f.len(), w * h * 3 / 2);
        // Top-left is the 75% white bar, the box centre is dark, and some caption pixels are bright.
        assert!((175..=185).contains(&f[0]));
        let centre_row = &f[(h / 2) * w..(h / 2 + 1) * w];
        assert!(centre_row[w / 2 - 300..w / 2 + 300].iter().any(|&y| y < 35));
        assert!(f[..w * h].iter().filter(|&&y| y > 210).count() > 1000);
    }

    #[test]
    fn odd_and_tiny_sizes_do_not_panic() {
        assert_eq!(render(33, 17, "X").len(), 32 * 16 * 3 / 2);
        render(2, 2, &Reason::SharingOff.caption());
    }

    #[test]
    fn folds_accents_for_the_font() {
        assert_eq!(fold("señal perdida"), "SENAL PERDIDA");
        assert_eq!(fold("Déconnecté"), "DECONNECTE");
    }
}
