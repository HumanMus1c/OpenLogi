//! The tray badge: a tone-coloured disc with a percentage, a charging bolt,
//! or a dash, rendered as RGBA at the shell's small-icon size.
//!
//! The renderer is pure and platform-free: the Win32 side only converts the
//! pixels into an `HICON`. All shapes are drawn at four times the target size
//! and box-downsampled, so the disc and glyph edges anti-alias without a
//! font or GDI dependency.

#![expect(
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "icon dimensions are bounded to 64 px and the supersampled canvas to 256 px; every integer is exact in f32 and every coordinate cast is a bounded non-negative value"
)]

use openlogi_core::device::{BatteryDevice, BatteryReading, BatteryTone};

/// The disc colour role. Unlike [`BatteryTone`], no reading is neutral, and a
/// healthy reading gets the brand accent instead of the primary text colour.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum BadgeTone {
    Accent,
    Green,
    Amber,
    Red,
    Neutral,
}

/// What sits inside the disc.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum BadgeText {
    /// The measured percentage, clamped to `0..=100`.
    Percent(u8),
    /// A bolt, for a charging device at either rate.
    Charging,
    /// A single dash, for a level-only or missing reading.
    Dash,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Badge {
    pub(crate) text: BadgeText,
    pub(crate) tone: BadgeTone,
}

impl Badge {
    /// The badge for `device`: digits when there is a percentage, a bolt while
    /// charging, otherwise a dash in the reading's tone.
    pub(crate) fn for_device(device: &BatteryDevice) -> Self {
        let Some(battery) = &device.battery else {
            return Self::none();
        };
        let tone = match battery.tone() {
            BatteryTone::Charging | BatteryTone::Full => BadgeTone::Green,
            BatteryTone::Low => BadgeTone::Amber,
            BatteryTone::Critical | BatteryTone::Error => BadgeTone::Red,
            BatteryTone::Normal => BadgeTone::Accent,
        };
        let text = if battery.tone() == BatteryTone::Charging {
            BadgeText::Charging
        } else {
            match battery.reading() {
                BatteryReading::Percent(percentage) => BadgeText::Percent(percentage.min(100)),
                BatteryReading::Level(_) | BatteryReading::NoReading => BadgeText::Dash,
            }
        };
        Self { text, tone }
    }

    /// The badge when no device reports anything: a neutral dash.
    pub(crate) const fn none() -> Self {
        Self {
            text: BadgeText::Dash,
            tone: BadgeTone::Neutral,
        }
    }
}

const ACCENT: [u8; 3] = [0x3B, 0x82, 0xF6];
const GREEN: [u8; 3] = [0x22, 0xC5, 0x5E];
const AMBER: [u8; 3] = [0xEA, 0xB3, 0x08];
const RED: [u8; 3] = [0xEF, 0x44, 0x44];
const NEUTRAL: [u8; 3] = [0x6B, 0x72, 0x80];
const WHITE: [u8; 3] = [0xFF, 0xFF, 0xFF];

/// Supersampling factor: shapes are drawn at `size * 4` and box-downsampled.
const SS: u32 = 4;

/// Our own 3x5 digit font, rows top to bottom. Deliberately hand-written for
/// this renderer; no third-party bitmap is involved.
const DIGITS: [[u8; 5]; 10] = [
    [0b111, 0b101, 0b101, 0b101, 0b111],
    [0b010, 0b110, 0b010, 0b010, 0b111],
    [0b111, 0b001, 0b111, 0b100, 0b111],
    [0b111, 0b001, 0b111, 0b001, 0b111],
    [0b101, 0b101, 0b111, 0b001, 0b001],
    [0b111, 0b100, 0b111, 0b001, 0b111],
    [0b111, 0b100, 0b111, 0b101, 0b111],
    [0b111, 0b001, 0b001, 0b001, 0b001],
    [0b111, 0b101, 0b111, 0b101, 0b111],
    [0b111, 0b101, 0b111, 0b001, 0b111],
];

/// A lightning silhouette in normalized coordinates, y pointing down.
const BOLT: [(f32, f32); 6] = [
    (0.18, -0.5),
    (-0.38, 0.08),
    (-0.02, 0.08),
    (-0.18, 0.5),
    (0.42, -0.12),
    (0.06, -0.12),
];

/// Render `badge` as straight-alpha RGBA at `size`×`size`.
#[must_use]
pub(crate) fn render(badge: Badge, size: u32, light_taskbar: bool) -> Vec<u8> {
    let size = size.clamp(8, 64);
    let n = size * SS;
    let mut pixels = vec![0u8; (n * n) as usize * 4];

    let disc = tone_rgb(badge.tone);
    let ring = if light_taskbar {
        [0x1D, 0x1D, 0x1F]
    } else {
        [0xF5, 0xF5, 0xF7]
    };
    let ring_alpha = if light_taskbar { 140 } else { 190 };

    let center = n as f32 / 2.0;
    let radius = center - SS as f32 * 0.75;
    let ring_width = SS as f32;
    let inner_radius = radius - ring_width;
    let glyph_scale = match badge.text {
        BadgeText::Percent(percentage) => digit_scale(percentage, inner_radius),
        BadgeText::Charging => inner_radius * 1.4,
        BadgeText::Dash => 0.0,
    };

    for y in 0..n {
        for x in 0..n {
            let dx = x as f32 + 0.5 - center;
            let dy = y as f32 + 0.5 - center;
            let distance = (dx * dx + dy * dy).sqrt();
            let (rgb, alpha) = if distance > radius {
                ([0, 0, 0], 0)
            } else if distance > inner_radius {
                (ring, ring_alpha)
            } else {
                (disc, 255)
            };
            let mut rgb = rgb;
            let mut alpha = alpha;
            if alpha > 0
                && distance <= inner_radius - SS as f32 * 0.5
                && glyph_hit(badge.text, glyph_scale, inner_radius, dx, dy)
            {
                rgb = WHITE;
                alpha = 255;
            }
            let offset = ((y * n + x) * 4) as usize;
            pixels[offset..offset + 4].copy_from_slice(&[rgb[0], rgb[1], rgb[2], alpha]);
        }
    }

    downsample(&pixels, n, size)
}

fn tone_rgb(tone: BadgeTone) -> [u8; 3] {
    match tone {
        BadgeTone::Accent => ACCENT,
        BadgeTone::Green => GREEN,
        BadgeTone::Amber => AMBER,
        BadgeTone::Red => RED,
        BadgeTone::Neutral => NEUTRAL,
    }
}

/// Pixels-per-font-pixel for the digits, fitted to the disc's inner circle.
fn digit_scale(percentage: u8, inner_radius: f32) -> f32 {
    let count = digit_count(percentage);
    let font_width = count as f32 * 3.0 + count.saturating_sub(1) as f32;
    let width_scale = 2.0 * inner_radius * 0.82 / font_width;
    let height_scale = 2.0 * inner_radius * 0.66 / 5.0;
    width_scale.min(height_scale)
}

const fn digit_count(percentage: u8) -> u32 {
    if percentage >= 100 {
        3
    } else if percentage >= 10 {
        2
    } else {
        1
    }
}

/// Whether the supersampled point at `(dx, dy)` from the disc centre is ink.
fn glyph_hit(text: BadgeText, scale: f32, inner_radius: f32, dx: f32, dy: f32) -> bool {
    match text {
        BadgeText::Percent(percentage) => digit_hit(percentage, scale, dx, dy),
        BadgeText::Charging => point_in_polygon(dx, dy, scale),
        BadgeText::Dash => {
            dx.abs() <= inner_radius * 0.42
                && dy.abs() <= (inner_radius * 0.12).max(SS as f32 * 0.9)
        }
    }
}

fn digit_hit(percentage: u8, scale: f32, dx: f32, dy: f32) -> bool {
    if scale <= 0.0 {
        return false;
    }
    let text = percentage.to_string();
    let count = text.len() as f32;
    let font_width = count * 3.0 + (count - 1.0);
    let total_width = font_width * scale;
    let total_height = 5.0 * scale;
    let left = -total_width / 2.0;
    let top = -total_height / 2.0;
    let local_x = dx - left;
    let local_y = dy - top;
    if local_x < 0.0 || local_y < 0.0 {
        return false;
    }
    let column = (local_x / scale) as usize;
    let row = (local_y / scale) as usize;
    if row >= 5 {
        return false;
    }
    let mut cursor = 0usize;
    for character in text.chars() {
        let digit = character.to_digit(10).unwrap_or(0) as usize;
        let digit_width = 3usize;
        if column >= cursor && column < cursor + digit_width {
            let bit = DIGITS[digit][row] >> (digit_width - 1 - (column - cursor)) & 1;
            return bit == 1;
        }
        cursor += digit_width + 1;
    }
    false
}

fn point_in_polygon(dx: f32, dy: f32, scale: f32) -> bool {
    let mut inside = false;
    let mut previous = BOLT[BOLT.len() - 1];
    for &current in &BOLT {
        let (x1, y1) = current;
        let (x2, y2) = previous;
        if (y1 > dy / scale) != (y2 > dy / scale) {
            let x = x1 + (dy / scale - y1) * (x2 - x1) / (y2 - y1);
            if dx / scale < x {
                inside = !inside;
            }
        }
        previous = current;
    }
    inside
}

/// Box-downsample premultiplied colour, then unpremultiply so edge pixels keep
/// their hue instead of fading towards black.
fn downsample(source: &[u8], n: u32, size: u32) -> Vec<u8> {
    let mut out = vec![0u8; (size * size) as usize * 4];
    let ss_area = SS * SS;
    for y in 0..size {
        for x in 0..size {
            let mut sum_r = 0u32;
            let mut sum_g = 0u32;
            let mut sum_b = 0u32;
            let mut sum_a = 0u32;
            for sy in 0..SS {
                for sx in 0..SS {
                    let offset = (((y * SS + sy) * n + x * SS + sx) * 4) as usize;
                    let alpha = u32::from(source[offset + 3]);
                    sum_r += u32::from(source[offset]) * alpha;
                    sum_g += u32::from(source[offset + 1]) * alpha;
                    sum_b += u32::from(source[offset + 2]) * alpha;
                    sum_a += alpha;
                }
            }
            let offset = ((y * size + x) * 4) as usize;
            let (Some(red), Some(green), Some(blue)) = (
                sum_r.checked_div(sum_a),
                sum_g.checked_div(sum_a),
                sum_b.checked_div(sum_a),
            ) else {
                out[offset..offset + 4].copy_from_slice(&[0, 0, 0, 0]);
                continue;
            };
            out[offset] = u8::try_from(red).unwrap_or(255);
            out[offset + 1] = u8::try_from(green).unwrap_or(255);
            out[offset + 2] = u8::try_from(blue).unwrap_or(255);
            out[offset + 3] = u8::try_from(sum_a / ss_area).unwrap_or(0);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::{Badge, BadgeText, BadgeTone, render};

    fn pixel(rgba: &[u8], size: u32, x: u32, y: u32) -> [u8; 4] {
        let offset = ((y * size + x) * 4) as usize;
        [
            rgba[offset],
            rgba[offset + 1],
            rgba[offset + 2],
            rgba[offset + 3],
        ]
    }

    fn count_white(rgba: &[u8]) -> usize {
        let (pixels, _) = rgba.as_chunks::<4>();
        pixels
            .iter()
            .filter(|pixel| pixel[3] > 200 && pixel[0] > 230 && pixel[1] > 230 && pixel[2] > 230)
            .count()
    }

    #[test]
    fn every_face_renders_at_every_tray_size() {
        let faces = [
            Badge {
                text: BadgeText::Percent(87),
                tone: BadgeTone::Accent,
            },
            Badge {
                text: BadgeText::Percent(15),
                tone: BadgeTone::Amber,
            },
            Badge {
                text: BadgeText::Charging,
                tone: BadgeTone::Green,
            },
            Badge {
                text: BadgeText::Dash,
                tone: BadgeTone::Red,
            },
            Badge::none(),
        ];
        for size in [16, 20, 24, 32] {
            for light in [true, false] {
                for face in faces {
                    let rgba = render(face, size, light);
                    assert_eq!(rgba.len(), (size * size * 4) as usize);
                    assert_eq!(pixel(&rgba, size, 0, 0)[3], 0, "{face:?} {size}");
                    assert_eq!(pixel(&rgba, size, size - 1, size - 1)[3], 0);
                    assert!(pixel(&rgba, size, size / 2, size / 2)[3] > 200);
                }
            }
        }
    }

    #[test]
    fn a_percentage_puts_white_digits_in_the_disc() {
        let digits = render(
            Badge {
                text: BadgeText::Percent(87),
                tone: BadgeTone::Accent,
            },
            16,
            false,
        );
        let dash = render(
            Badge {
                text: BadgeText::Dash,
                tone: BadgeTone::Accent,
            },
            16,
            false,
        );
        assert!(count_white(&digits) > count_white(&dash));
    }

    #[test]
    fn charging_and_none_have_distinct_faces() {
        let bolt = render(
            Badge {
                text: BadgeText::Charging,
                tone: BadgeTone::Green,
            },
            20,
            false,
        );
        let dash = render(
            Badge {
                text: BadgeText::Dash,
                tone: BadgeTone::Neutral,
            },
            20,
            false,
        );
        assert!(count_white(&bolt) > 0);
        assert!(count_white(&dash) > 0);
    }

    #[test]
    fn the_disc_keeps_its_tone_and_the_ring_follows_the_taskbar() {
        let face = Badge {
            text: BadgeText::Percent(42),
            tone: BadgeTone::Green,
        };
        let size = 24;
        let dark = render(face, size, false);
        let light = render(face, size, true);
        // A disc sample left of the digits.
        let disc = pixel(&dark, size, size / 2, size - 4);
        assert!(disc[3] > 200);
        assert!(
            disc[1] > disc[0] && disc[1] > disc[2],
            "green disc: {disc:?}"
        );
        // The outer ring flips between taskbars.
        assert_ne!(
            pixel(&dark, size, size - 2, size / 2),
            pixel(&light, size, size - 2, size / 2)
        );
    }

    #[test]
    fn no_reading_is_neutral() {
        let face = Badge::none();
        assert_eq!(face.tone, BadgeTone::Neutral);
        assert_eq!(face.text, BadgeText::Dash);
    }
}
