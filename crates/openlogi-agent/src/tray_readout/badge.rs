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

use openlogi_core::config::TrayBadgeStyle;
#[cfg(any(target_os = "linux", test))]
use openlogi_core::device::BatteryInfo;
#[cfg(any(target_os = "windows", target_os = "linux", test))]
use openlogi_core::device::{BatteryColor, BatteryDevice, BatteryReading, BatteryTone};

/// The disc colour role. Unlike [`BatteryTone`], a healthy *percentage* gets
/// the green the three-stage band gives it; the brand accent is left for a
/// level-only healthy reading, which has no percentage to band.
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
    /// charging, otherwise a dash — painted with the shared colour role, so a
    /// percentage shows its three-stage band (green from 70%, amber from 40%,
    /// red below) and a level-only reading keeps its firmware tone.
    #[cfg(any(target_os = "windows", target_os = "linux", test))]
    pub(crate) fn for_device(device: &BatteryDevice) -> Self {
        let Some(battery) = &device.battery else {
            return Self::none();
        };
        let tone = match battery.color() {
            BatteryColor::Green => BadgeTone::Green,
            BatteryColor::Amber => BadgeTone::Amber,
            BatteryColor::Red => BadgeTone::Red,
            BatteryColor::Neutral => BadgeTone::Accent,
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

/// The letters and marks the wide face sets its name with, in the same 3x5
/// cell as [`DIGITS`]. Hand-written for this renderer like the digits.
const LETTERS: [(char, [u8; 5]); 26] = [
    ('A', [0b010, 0b101, 0b111, 0b101, 0b101]),
    ('B', [0b110, 0b101, 0b110, 0b101, 0b110]),
    ('C', [0b011, 0b100, 0b100, 0b100, 0b011]),
    ('D', [0b110, 0b101, 0b101, 0b101, 0b110]),
    ('E', [0b111, 0b100, 0b110, 0b100, 0b111]),
    ('F', [0b111, 0b100, 0b110, 0b100, 0b100]),
    ('G', [0b011, 0b100, 0b101, 0b101, 0b011]),
    ('H', [0b101, 0b101, 0b111, 0b101, 0b101]),
    ('I', [0b111, 0b010, 0b010, 0b010, 0b111]),
    ('J', [0b001, 0b001, 0b001, 0b101, 0b010]),
    ('K', [0b101, 0b101, 0b110, 0b101, 0b101]),
    ('L', [0b100, 0b100, 0b100, 0b100, 0b111]),
    ('M', [0b101, 0b111, 0b111, 0b101, 0b101]),
    ('N', [0b101, 0b111, 0b101, 0b101, 0b101]),
    ('O', [0b010, 0b101, 0b101, 0b101, 0b010]),
    ('P', [0b110, 0b101, 0b110, 0b100, 0b100]),
    ('Q', [0b010, 0b101, 0b101, 0b110, 0b011]),
    ('R', [0b110, 0b101, 0b110, 0b101, 0b101]),
    ('S', [0b011, 0b100, 0b010, 0b001, 0b110]),
    ('T', [0b111, 0b010, 0b010, 0b010, 0b010]),
    ('U', [0b101, 0b101, 0b101, 0b101, 0b111]),
    ('V', [0b101, 0b101, 0b101, 0b101, 0b010]),
    ('W', [0b101, 0b101, 0b111, 0b111, 0b101]),
    ('X', [0b101, 0b101, 0b010, 0b101, 0b101]),
    ('Y', [0b101, 0b101, 0b010, 0b010, 0b010]),
    ('Z', [0b111, 0b001, 0b010, 0b100, 0b111]),
];

/// The glyph for `character`, or `None` when the font cannot draw it.
fn glyph_rows(character: char) -> Option<[u8; 5]> {
    if let Some(digit) = character.to_digit(10) {
        return Some(DIGITS[digit as usize]);
    }
    match character {
        ' ' => Some([0; 5]),
        '-' => Some([0b000, 0b000, 0b111, 0b000, 0b000]),
        '.' => Some([0b000, 0b000, 0b000, 0b000, 0b010]),
        '%' => Some([0b101, 0b001, 0b010, 0b100, 0b101]),
        _ => LETTERS
            .iter()
            .find(|(glyph, _)| *glyph == character.to_ascii_uppercase())
            .map(|(_, rows)| *rows),
    }
}

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
pub(crate) fn render(
    badge: Badge,
    size: u32,
    light_taskbar: bool,
    style: TrayBadgeStyle,
) -> Vec<u8> {
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

    let half = n as f32 / 2.0;
    let ring_width = SS as f32;
    // A quarter of the side, so a 20 px square badge rounds like the
    // squircle-shaped icons around it rather than like a squircle pill.
    let corner = half / 2.0;
    // Both shapes keep the glyphs fitted to the inner circle: a square's
    // inscribed circle equals the disc's post-ring edge, so the digits read
    // the same in either shape and never graze the ring or a corner.
    let fit_radius = half - ring_width;
    let glyph_scale = match badge.text {
        BadgeText::Percent(percentage) => digit_scale(percentage, fit_radius),
        BadgeText::Charging => fit_radius * 1.4,
        BadgeText::Dash => 0.0,
    };

    for y in 0..n {
        for x in 0..n {
            let dx = x as f32 + 0.5 - half;
            let dy = y as f32 + 0.5 - half;
            // Signed distance to the shape's edge: negative inside, so the
            // ring band and glyph test stay shape-agnostic.
            let edge = match style {
                // The wide face is rendered by `render_wide`; a caller that
                // still reaches this square renderer with it gets the disc.
                TrayBadgeStyle::Circle | TrayBadgeStyle::Wide => (dx * dx + dy * dy).sqrt() - half,
                TrayBadgeStyle::RoundedSquare => rounded_square_edge(dx, dy, half, corner),
            };
            let (rgb, alpha) = if edge >= 0.0 {
                ([0, 0, 0], 0)
            } else if edge > -ring_width {
                (ring, ring_alpha)
            } else {
                (disc, 255)
            };
            let mut rgb = rgb;
            let mut alpha = alpha;
            if alpha > 0
                && edge <= -ring_width - SS as f32 * 0.5
                && glyph_hit(badge.text, glyph_scale, fit_radius, dx, dy)
            {
                rgb = WHITE;
                alpha = 255;
            }
            let offset = ((y * n + x) * 4) as usize;
            pixels[offset..offset + 4].copy_from_slice(&[rgb[0], rgb[1], rgb[2], alpha]);
        }
    }

    downsample(&pixels, n, size, size)
}

/// The signed distance to a centred rounded square's edge: the standard
/// rounded-rectangle SDF, negative inside.
fn rounded_square_edge(dx: f32, dy: f32, half: f32, corner: f32) -> f32 {
    rounded_rect_edge(dx, dy, half, half, corner)
}

/// The signed distance to a centred rounded rectangle's edge, negative
/// inside. `corner` is capped at the shorter half-extent.
fn rounded_rect_edge(dx: f32, dy: f32, half_width: f32, half_height: f32, corner: f32) -> f32 {
    let corner = corner.min(half_width).min(half_height);
    let qx = dx.abs() - (half_width - corner);
    let qy = dy.abs() - (half_height - corner);
    let (outside_x, outside_y) = (qx.max(0.0), qy.max(0.0));
    (outside_x * outside_x + outside_y * outside_y).sqrt() + qx.max(qy).min(0.0) - corner
}

/// The most name characters the wide face draws, so a long product name
/// cannot grow the panel item without bound.
#[cfg(any(target_os = "linux", test))]
const WIDE_NAME_MAX: usize = 14;

/// A wide badge face: the device name, a progress bar, and the value in one
/// row. A panel that keeps a pixmap's aspect ratio can show all three; the
/// square faces cannot.
#[cfg(any(target_os = "linux", test))]
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct WideBadge {
    pub(crate) name: Option<String>,
    pub(crate) text: BadgeText,
    /// How full the progress bar draws, from the same reading the settings
    /// app bars use. `None` for an error or a missing reading.
    pub(crate) fill: Option<u8>,
    pub(crate) tone: BadgeTone,
}

#[cfg(any(target_os = "linux", test))]
impl WideBadge {
    /// The wide face for `device`, matching the badge the square faces show.
    pub(crate) fn for_device(device: &BatteryDevice) -> Self {
        let badge = Badge::for_device(device);
        let battery = device.battery.as_ref();
        let fill = battery
            .filter(|battery| battery.tone() != BatteryTone::Error)
            .and_then(BatteryInfo::bar_percentage)
            .map(|percentage| percentage.min(100));
        Self {
            name: device.name.clone(),
            text: badge.text,
            fill,
            tone: badge.tone,
        }
    }

    /// The wide face when no device reports anything.
    pub(crate) const fn none() -> Self {
        Self {
            name: None,
            text: BadgeText::Dash,
            fill: None,
            tone: BadgeTone::Neutral,
        }
    }

    /// The name as the hand-written font can draw: ASCII only, upper-cased,
    /// unsupported characters collapsed to a space, and cut to
    /// [`WIDE_NAME_MAX`]. `None` when nothing drawable remains, so a
    /// localized or CJK name falls back to the bar and value alone.
    fn drawable_name(&self) -> Option<String> {
        let name = self.name.as_deref()?.trim();
        if name.is_empty() || !name.is_ascii() {
            return None;
        }
        let mut drawable = String::with_capacity(name.len());
        for character in name.chars() {
            let character = character.to_ascii_uppercase();
            let character = if glyph_rows(character).is_some() {
                character
            } else {
                ' '
            };
            if character == ' ' && drawable.ends_with(' ') {
                continue;
            }
            drawable.push(character);
        }
        let drawable: String = drawable.trim().chars().take(WIDE_NAME_MAX).collect();
        let drawable = drawable.trim_end();
        (!drawable.is_empty()).then(|| drawable.to_owned())
    }

    /// The value the wide face prints: the digits and a percent sign, or the
    /// dash for a missing reading. A charging cold start prints nothing —
    /// the bolt takes the value slot.
    fn value_text(&self) -> Option<String> {
        match self.text {
            BadgeText::Percent(percentage) => Some(format!("{percentage}%")),
            BadgeText::Charging => None,
            BadgeText::Dash => Some("-".to_owned()),
        }
    }
}

/// Render `badge` as a wide face `height` pixels tall, returning the width it
/// needed and straight-alpha RGBA pixels.
#[cfg(any(target_os = "linux", test))]
#[must_use]
pub(crate) fn render_wide(badge: &WideBadge, height: u32, light_panel: bool) -> (u32, Vec<u8>) {
    let height = height.clamp(8, 64);
    let n = height * SS;
    // The face is a rounded rectangle rather than a pill: a pill's end caps
    // are half the height wide, and text inside them gets clipped.
    let pad = n as f32 * 0.16;
    let corner = n as f32 * 0.22;
    let gap = (n as f32 * 0.10).max(SS as f32);
    let name_scale = n as f32 * 0.085;
    let value_scale = n as f32 * 0.078;
    let bar_height = (n as f32 * 0.14).max(SS as f32);
    let bar_width = n as f32 * 1.1;
    let bolt_scale = n as f32 * 0.10;

    let name = badge.drawable_name();
    let value = badge.value_text();
    let bolt = badge.text == BadgeText::Charging;
    let name_width = name
        .as_deref()
        .map_or(0.0, |name| text_width(name, name_scale));
    let value_width = value
        .as_deref()
        .map_or(0.0, |value| text_width(value, value_scale));
    let value_slot = if bolt { bolt_scale * 0.85 } else { value_width };
    let name_gap = if name_width > 0.0 { gap } else { 0.0 };
    let total = (pad * 2.0 + name_width + name_gap + bar_width + gap * 0.6 + value_slot)
        .max(n as f32 * 2.5);
    let width = ((total / SS as f32).ceil() as u32).max(8);
    let canvas_width = width * SS;

    let disc = tone_rgb(badge.tone);
    let track = mix(disc, WHITE, 0.35);
    let ring = if light_panel {
        [0x1D, 0x1D, 0x1F]
    } else {
        [0xF5, 0xF5, 0xF7]
    };
    let ring_alpha = if light_panel { 140 } else { 190 };
    let ring_width = SS as f32;
    let half_width = canvas_width as f32 / 2.0;
    let half_height = n as f32 / 2.0;

    let name_left = pad;
    let bar_left = name_left + name_width + name_gap;
    let bar_center_x = bar_left + bar_width / 2.0;
    let value_left = bar_left + bar_width + gap * 0.6;
    let name_top = half_height - 5.0 * name_scale / 2.0;
    let value_top = half_height - 5.0 * value_scale / 2.0;
    let bolt_center_x = value_left + bolt_scale * 0.85 / 2.0;
    let fill_fraction = f32::from(badge.fill.unwrap_or(0)) / 100.0;

    let mut pixels = vec![0u8; (canvas_width * n) as usize * 4];
    for y in 0..n {
        for x in 0..canvas_width {
            let px = x as f32 + 0.5;
            let py = y as f32 + 0.5;
            let dx = px - half_width;
            let dy = py - half_height;
            let edge = rounded_rect_edge(dx, dy, half_width, half_height, corner);
            let (rgb, alpha) = if edge >= 0.0 {
                ([0, 0, 0], 0)
            } else if edge > -ring_width {
                (ring, ring_alpha)
            } else {
                (disc, 255)
            };
            let mut rgb = rgb;
            if alpha > 0 && edge <= -ring_width - SS as f32 * 0.5 {
                let in_track = rounded_rect_edge(
                    px - bar_center_x,
                    py - half_height,
                    bar_width / 2.0,
                    bar_height / 2.0,
                    bar_height / 2.0,
                ) <= 0.0;
                let name_ink = name
                    .as_deref()
                    .is_some_and(|name| text_hit(name, name_scale, name_left, name_top, px, py));
                let value_ink = value.as_deref().is_some_and(|value| {
                    text_hit(value, value_scale, value_left, value_top, px, py)
                });
                let bolt_ink =
                    bolt && point_in_polygon(px - bolt_center_x, py - half_height, bolt_scale);
                if in_track {
                    rgb = if px <= bar_left + bar_width * fill_fraction {
                        WHITE
                    } else {
                        track
                    };
                } else if name_ink || value_ink || bolt_ink {
                    rgb = WHITE;
                }
            }
            let offset = ((y * canvas_width + x) * 4) as usize;
            pixels[offset..offset + 4].copy_from_slice(&[rgb[0], rgb[1], rgb[2], alpha]);
        }
    }

    (width, downsample(&pixels, canvas_width, width, height))
}

/// Blend `amount` of `over` into `base` per channel.
#[cfg(any(target_os = "linux", test))]
fn mix(base: [u8; 3], over: [u8; 3], amount: f32) -> [u8; 3] {
    let blend =
        |base: u8, over: u8| (f32::from(base) * (1.0 - amount) + f32::from(over) * amount) as u8;
    [
        blend(base[0], over[0]),
        blend(base[1], over[1]),
        blend(base[2], over[2]),
    ]
}

/// The pixel width of a text run in the hand-written font.
fn text_width(text: &str, scale: f32) -> f32 {
    let count = text.chars().count() as f32;
    (count * 3.0 + (count - 1.0).max(0.0)) * scale
}

/// Whether `(px, py)` is ink of `text`, whose run starts at `left`/`top`.
fn text_hit(text: &str, scale: f32, left: f32, top: f32, px: f32, py: f32) -> bool {
    if scale <= 0.0 {
        return false;
    }
    let local_x = px - left;
    let local_y = py - top;
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
        if column >= cursor && column < cursor + 3 {
            let Some(rows) = glyph_rows(character) else {
                return false;
            };
            return rows[row] >> (2 - (column - cursor)) & 1 == 1;
        }
        cursor += 4;
    }
    false
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
    let text = percentage.to_string();
    let left = -text_width(&text, scale) / 2.0;
    let top = -5.0 * scale / 2.0;
    text_hit(&text, scale, left, top, dx, dy)
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
/// their hue instead of fading towards black. The source canvas is
/// `canvas_width`×`canvas_height` supersampled pixels; the result is
/// `width`×`height`.
fn downsample(source: &[u8], canvas_width: u32, width: u32, height: u32) -> Vec<u8> {
    let mut out = vec![0u8; (width * height) as usize * 4];
    let ss_area = SS * SS;
    for y in 0..height {
        for x in 0..width {
            let mut sum_r = 0u32;
            let mut sum_g = 0u32;
            let mut sum_b = 0u32;
            let mut sum_a = 0u32;
            for sy in 0..SS {
                for sx in 0..SS {
                    let offset = (((y * SS + sy) * canvas_width + x * SS + sx) * 4) as usize;
                    let alpha = u32::from(source[offset + 3]);
                    sum_r += u32::from(source[offset]) * alpha;
                    sum_g += u32::from(source[offset + 1]) * alpha;
                    sum_b += u32::from(source[offset + 2]) * alpha;
                    sum_a += alpha;
                }
            }
            let offset = ((y * width + x) * 4) as usize;
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
    use openlogi_core::config::TrayBadgeStyle;

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
                for style in [TrayBadgeStyle::Circle, TrayBadgeStyle::RoundedSquare] {
                    for face in faces {
                        let rgba = render(face, size, light, style);
                        assert_eq!(rgba.len(), (size * size * 4) as usize);
                        // Both shapes round their corners: the square's
                        // quarter-side radius keeps the bitmap corner clear.
                        assert_eq!(pixel(&rgba, size, 0, 0)[3], 0, "{face:?} {size} {style:?}");
                        assert_eq!(pixel(&rgba, size, size - 1, size - 1)[3], 0);
                        assert!(pixel(&rgba, size, size / 2, size / 2)[3] > 200);
                    }
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
            TrayBadgeStyle::Circle,
        );
        let dash = render(
            Badge {
                text: BadgeText::Dash,
                tone: BadgeTone::Accent,
            },
            16,
            false,
            TrayBadgeStyle::Circle,
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
            TrayBadgeStyle::Circle,
        );
        let dash = render(
            Badge {
                text: BadgeText::Dash,
                tone: BadgeTone::Neutral,
            },
            20,
            false,
            TrayBadgeStyle::Circle,
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
        let dark = render(face, size, false, TrayBadgeStyle::Circle);
        let light = render(face, size, true, TrayBadgeStyle::Circle);
        // A disc sample left of the digits.
        let disc = pixel(&dark, size, size / 2, size - 4);
        assert!(disc[3] > 200);
        assert!(
            disc[1] > disc[0] && disc[1] > disc[2],
            "green disc: {disc:?}"
        );
        // The outer ring flips between taskbars: the last pixel column sits
        // inside the full-bleed disc's one-pixel ring band.
        assert_ne!(
            pixel(&dark, size, size - 1, size / 2),
            pixel(&light, size, size - 1, size / 2)
        );
    }

    #[test]
    fn the_square_style_fills_where_the_circle_stays_clear() {
        let face = Badge {
            text: BadgeText::Percent(42),
            tone: BadgeTone::Accent,
        };
        let size = 24;
        let circle = render(face, size, false, TrayBadgeStyle::Circle);
        let square = render(face, size, false, TrayBadgeStyle::RoundedSquare);
        // Inside the corner band the square is inked where the inscribed
        // circle has already ended: the square carries the visual weight of
        // the square-ish icons around it.
        let (x, y) = (3, 1);
        assert_eq!(pixel(&circle, size, x, y)[3], 0, "circle corner band");
        assert!(pixel(&square, size, x, y)[3] > 200, "square corner band");
    }

    #[test]
    fn no_reading_is_neutral() {
        let face = Badge::none();
        assert_eq!(face.tone, BadgeTone::Neutral);
        assert_eq!(face.text, BadgeText::Dash);
    }

    fn named_device(
        name: Option<&str>,
        percentage: Option<u8>,
        level: openlogi_core::device::BatteryLevel,
        status: openlogi_core::device::BatteryStatus,
    ) -> openlogi_core::device::BatteryDevice {
        use openlogi_core::device::{BatteryDevice, BatteryInfo, DeviceKind};

        BatteryDevice {
            name: name.map(str::to_owned),
            kind: DeviceKind::Mouse,
            online: true,
            battery: Some(BatteryInfo {
                percentage,
                level,
                status,
            }),
        }
    }

    fn device(
        percentage: Option<u8>,
        level: openlogi_core::device::BatteryLevel,
        status: openlogi_core::device::BatteryStatus,
    ) -> openlogi_core::device::BatteryDevice {
        named_device(Some("MX Master 3S"), percentage, level, status)
    }

    #[test]
    fn the_wide_face_keeps_its_height_and_earns_its_width() {
        use super::{WIDE_NAME_MAX, WideBadge, render_wide};
        use openlogi_core::device::{BatteryLevel, BatteryStatus};

        let good = BatteryLevel::Good;
        let discharging = BatteryStatus::Discharging;
        let face = WideBadge::for_device(&named_device(
            Some("MX Master 3S"),
            Some(64),
            good,
            discharging,
        ));
        assert_eq!(face.text, BadgeText::Percent(64));
        assert_eq!(face.fill, Some(64));

        let (width, pixels) = render_wide(&face, 22, false);
        assert!(width > 22, "a name and a bar need width: {width}");
        assert_eq!(pixels.len(), (width * 22 * 4) as usize);
        // Opaque inside the pill, transparent outside its corners.
        assert!(pixel(&pixels, width, 1, 11)[3] > 200);
        assert_eq!(pixel(&pixels, width, 0, 0)[3], 0);

        // A double-height face is the same face, scaled.
        let (_, tall) = render_wide(&face, 44, false);
        assert!(tall.len() > pixels.len());

        // A CJK name is not drawable in the hand-written font and drops out;
        // the bar and value stay.
        let cjk = WideBadge::for_device(&named_device(
            Some("办公室键盘"),
            Some(64),
            good,
            discharging,
        ));
        let (cjk_width, _) = render_wide(&cjk, 22, false);
        assert!(cjk_width < width, "{cjk_width} vs {width}");

        // The no-device face is a neutral dash pill, not an empty bitmap.
        let (none_width, none_pixels) = render_wide(&WideBadge::none(), 22, false);
        assert!(none_width >= 22);
        assert!(pixel(&none_pixels, none_width, 1, 11)[3] > 200);

        // A long name is cut to the cap, so the width cannot run away.
        let long = "A".repeat(64);
        let capped = "A".repeat(WIDE_NAME_MAX);
        let width_of = |name: &str| {
            let face =
                WideBadge::for_device(&named_device(Some(name), Some(64), good, discharging));
            render_wide(&face, 22, false).0
        };
        assert_eq!(width_of(&long), width_of(&capped));
    }

    #[test]
    fn a_wider_bar_fills_more_of_the_track() {
        use super::{WideBadge, render_wide};
        use openlogi_core::device::{BatteryLevel, BatteryStatus};

        let good = BatteryLevel::Good;
        let discharging = BatteryStatus::Discharging;
        let white_in_middle_row = |percentage| {
            let face =
                WideBadge::for_device(&named_device(None, Some(percentage), good, discharging));
            let (width, pixels) = render_wide(&face, 22, false);
            let (chunks, _) = pixels.as_chunks::<4>();
            (0..width)
                .filter(|&x| {
                    let pixel = &chunks[(11 * width + x) as usize];
                    pixel[3] > 200 && pixel[0] > 230 && pixel[1] > 230 && pixel[2] > 230
                })
                .count()
        };
        assert!(white_in_middle_row(95) > white_in_middle_row(5));
    }

    #[test]
    fn a_charging_wide_face_keeps_the_bolt_and_the_bar() {
        use super::{WideBadge, render_wide};
        use openlogi_core::device::{BatteryLevel, BatteryStatus};

        let face = WideBadge::for_device(&named_device(
            Some("MX Master 3S"),
            Some(45),
            BatteryLevel::Good,
            BatteryStatus::Charging,
        ));
        assert_eq!(face.text, BadgeText::Charging);
        assert_eq!(face.fill, Some(45));
        let (width, pixels) = render_wide(&face, 22, false);
        assert_eq!(pixels.len(), (width * 22 * 4) as usize);
        assert!(pixels.contains(&255));
    }

    #[test]
    fn a_percentage_badge_paints_its_three_stage_band() {
        use openlogi_core::device::{BatteryLevel, BatteryStatus};

        let good = BatteryLevel::Good;
        let discharging = BatteryStatus::Discharging;
        for (percentage, tone) in [
            (100, BadgeTone::Green),
            (70, BadgeTone::Green),
            (69, BadgeTone::Amber),
            (40, BadgeTone::Amber),
            (39, BadgeTone::Red),
            (1, BadgeTone::Red),
        ] {
            let badge = Badge::for_device(&device(Some(percentage), good, discharging));
            assert_eq!(
                badge,
                Badge {
                    text: BadgeText::Percent(percentage),
                    tone,
                },
                "{percentage}%"
            );
        }

        // Charging stays the green bolt whatever the band says; a fault is
        // red; a level-only healthy reading keeps the brand accent.
        assert_eq!(
            Badge::for_device(&device(Some(45), good, BatteryStatus::Charging)),
            Badge {
                text: BadgeText::Charging,
                tone: BadgeTone::Green,
            }
        );
        assert_eq!(
            Badge::for_device(&device(Some(85), good, BatteryStatus::Error)).tone,
            BadgeTone::Red
        );
        assert_eq!(
            Badge::for_device(&device(None, BatteryLevel::Good, discharging)),
            Badge {
                text: BadgeText::Dash,
                tone: BadgeTone::Accent,
            }
        );
        assert_eq!(
            Badge::for_device(&device(None, BatteryLevel::Low, discharging)).tone,
            BadgeTone::Amber
        );
        assert_eq!(
            Badge::for_device(&device(None, BatteryLevel::Critical, discharging)).tone,
            BadgeTone::Red
        );
    }
}
