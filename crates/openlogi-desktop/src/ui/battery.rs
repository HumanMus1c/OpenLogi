//! Theme-aware battery visuals and readouts.
//!
//! The device gallery, detail header, and device-information panel all use
//! this component so charge thresholds, status labels, and the cold-start
//! charging state stay visually consistent.

use gpui::{
    App, BorderStyle, Bounds, Corners, FontWeight, Hsla, IntoElement, ParentElement, PathBuilder,
    RenderOnce, Styled, Window, canvas, point, prelude::FluentBuilder as _, px, quad, rgb, size,
};
use gpui_kit::component::h_flex;
use openlogi_core::device::{BatteryInfo, BatteryStatus, BatteryTone, BatteryValue};

use super::theme::{self, Palette, Typography as _};

/// A battery readout with presentations sized for each desktop context.
#[derive(IntoElement)]
pub(crate) struct BatteryIndicator {
    battery: BatteryInfo,
    presentation: Presentation,
}

#[derive(Clone, Copy)]
enum Presentation {
    Inline,
    Glance,
    Status { online: bool },
    Summary,
}

impl BatteryIndicator {
    /// A terse glyph and value for the device-detail title bar.
    pub(crate) fn inline(battery: &BatteryInfo) -> Self {
        Self {
            battery: battery.clone(),
            presentation: Presentation::Inline,
        }
    }

    /// The tersest readout — glyph and value only — for a card corner whose
    /// context rides a hover tip (see [`Self::status`]).
    pub(crate) fn glance(battery: &BatteryInfo) -> Self {
        Self {
            battery: battery.clone(),
            presentation: Presentation::Glance,
        }
    }

    /// A gallery-card readout with status and last-known context.
    pub(crate) fn status(battery: &BatteryInfo, online: bool) -> Self {
        Self {
            battery: battery.clone(),
            presentation: Presentation::Status { online },
        }
    }

    /// A larger readout for the device-information panel.
    pub(crate) fn summary(battery: &BatteryInfo) -> Self {
        Self {
            battery: battery.clone(),
            presentation: Presentation::Summary,
        }
    }
}

impl RenderOnce for BatteryIndicator {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let pal = theme::palette(cx);
        match self.presentation {
            Presentation::Inline => inline_readout(&self.battery, pal).into_any_element(),
            Presentation::Glance => glance_readout(&self.battery, pal).into_any_element(),
            Presentation::Status { online } => {
                status_readout(&self.battery, online, pal).into_any_element()
            }
            Presentation::Summary => summary_readout(&self.battery, pal).into_any_element(),
        }
    }
}

fn inline_readout(battery: &BatteryInfo, pal: Palette) -> impl IntoElement {
    h_flex()
        .items_center()
        .gap_2()
        .text_caption()
        .child(battery_glyph(battery, pal, GlyphSize::Compact, pal.page))
        .child(
            gpui::div()
                .font_weight(FontWeight::MEDIUM)
                .text_color(pal.text_primary)
                .child(ValueSlot::new(battery)),
        )
}

fn glance_readout(battery: &BatteryInfo, pal: Palette) -> impl IntoElement {
    h_flex()
        .items_center()
        .gap_2()
        .text_caption()
        .child(battery_glyph(battery, pal, GlyphSize::Compact, pal.panel))
    // The number is hidden for now — the glyph's fill carries it, and
    // [`glance_hint`] repeats it on hover. Re-enable if the glyph alone
    // proves too coarse.
    // .child(
    //     gpui::div()
    //         .font_weight(FontWeight::MEDIUM)
    //         .text_color(pal.text_primary)
    //         .child(value_label(battery)),
    // )
}

fn status_readout(battery: &BatteryInfo, online: bool, pal: Palette) -> impl IntoElement {
    h_flex()
        .w_full()
        .items_center()
        .gap_2()
        .text_caption()
        .child(battery_glyph(battery, pal, GlyphSize::Compact, pal.panel))
        .child(
            gpui::div()
                .font_weight(FontWeight::MEDIUM)
                .text_color(pal.text_primary)
                .child(ValueSlot::new(battery)),
        )
        .when_some(context_label(battery, online), |row, label| {
            row.child(gpui::div().text_color(pal.text_muted).child(label))
        })
}

fn summary_readout(battery: &BatteryInfo, pal: Palette) -> impl IntoElement {
    h_flex()
        .w_full()
        .items_center()
        .gap_4()
        .child(battery_glyph(battery, pal, GlyphSize::Summary, pal.panel))
        .child(
            gpui::div()
                .text_heading()
                .text_color(pal.text_primary)
                .child(ValueSlot::new(battery)),
        )
        .child(
            gpui::div()
                .text_caption()
                .text_color(pal.text_muted)
                .child(summary_label(battery)),
        )
}

/// [`value_label`] in a slot at least as wide as the widest percentage,
/// right-aligned. A falling charge (74% → 71%, 10% → 9%) then moves neither
/// the glyph before the value nor the words after it. The width is measured
/// in the text style the slot renders in, so each presentation reserves its
/// own.
#[derive(IntoElement)]
struct ValueSlot {
    label: String,
}

impl ValueSlot {
    fn new(battery: &BatteryInfo) -> Self {
        Self {
            label: value_label(battery),
        }
    }
}

impl RenderOnce for ValueSlot {
    fn render(self, window: &mut Window, _cx: &mut App) -> impl IntoElement {
        let widest = gpui::SharedString::from(percent_label(100));
        let style = window.text_style();
        let run = style.to_run(widest.len());
        let width = window
            .text_system()
            .shape_line(
                widest,
                style.font_size.to_pixels(window.rem_size()),
                &[run],
                None,
            )
            .width
            // A text element lays out at its shaped width rounded up.
            .ceil();
        h_flex()
            .flex_none()
            .min_w(width)
            .justify_end()
            .child(self.label)
    }
}

fn percent_label(percentage: u8) -> String {
    format!("{percentage}%")
}

/// The headline value: the percentage, or the level's word for a device
/// that reports no percentage.
pub(crate) fn value_label(battery: &BatteryInfo) -> String {
    match battery.value() {
        BatteryValue::Percent(percentage) => percent_label(percentage),
        value => value
            .translation_key()
            .map_or_else(|| "—".to_owned(), |key| tr!(key).to_string()),
    }
}

/// The status word shown beside [`value_label`], unless it would only
/// repeat it (a level-only "Full" device that reports a full status).
fn secondary_label(battery: &BatteryInfo) -> Option<gpui::SharedString> {
    let label = match battery.status {
        BatteryStatus::Charging | BatteryStatus::ChargingSlow => Some(tr!("device.charging")),
        BatteryStatus::Full => Some(tr!("device.full")),
        BatteryStatus::Error => Some(tr!("device.battery_error")),
        BatteryStatus::Discharging | BatteryStatus::Unknown => {
            battery.needs_attention().then(|| tr!("device.low_battery"))
        }
    };
    label.filter(|label| label.as_ref() != value_label(battery))
}

/// Everything a [`BatteryIndicator::glance`] does not show — the value plus
/// the status and last-known words — for its caller's hover tip.
pub(crate) fn glance_hint(battery: &BatteryInfo, online: bool) -> String {
    match context_label(battery, online) {
        Some(context) => format!("{} · {context}", value_label(battery)),
        None => value_label(battery),
    }
}

fn context_label(battery: &BatteryInfo, online: bool) -> Option<String> {
    match (!online, secondary_label(battery)) {
        (true, Some(status)) => Some(format!("{} · {status}", tr!("device.last_known_battery"))),
        (true, None) => Some(tr!("device.last_known_battery").to_string()),
        (false, Some(status)) => Some(status.to_string()),
        (false, None) => None,
    }
}

fn summary_label(battery: &BatteryInfo) -> gpui::SharedString {
    if battery.charging_without_reading() {
        return tr!("device.battery");
    }
    let label = match battery.status {
        BatteryStatus::Charging | BatteryStatus::ChargingSlow => tr!("device.charging"),
        BatteryStatus::Full => tr!("device.full"),
        BatteryStatus::Error => tr!("device.battery_error"),
        BatteryStatus::Discharging | BatteryStatus::Unknown => {
            if battery.needs_attention() {
                tr!("device.low_battery")
            } else {
                tr!("device.battery")
            }
        }
    };
    // The caption names the reading; it must not repeat the headline.
    if label.as_ref() == value_label(battery) {
        tr!("device.battery")
    } else {
        label
    }
}

fn tone_color(tone: BatteryTone, pal: Palette) -> Hsla {
    match tone {
        BatteryTone::Charging | BatteryTone::Full => rgb(theme::STATUS_CONNECTED).into(),
        BatteryTone::Low => rgb(theme::STATUS_CONNECTING).into(),
        BatteryTone::Critical | BatteryTone::Error => rgb(theme::STATUS_DISABLED).into(),
        BatteryTone::Normal => pal.text_primary,
    }
}

#[derive(Clone, Copy)]
enum GlyphSize {
    Compact,
    Summary,
}

impl GlyphSize {
    const fn dimensions(self) -> (f32, f32) {
        match self {
            Self::Compact => (27., 14.),
            Self::Summary => (46., 22.),
        }
    }
}

#[derive(Clone, Copy)]
enum GlyphMark {
    None,
    Charging,
    Error,
}

#[derive(Clone, Copy)]
struct GlyphPaint {
    percentage: Option<u8>,
    track: Hsla,
    fill: Hsla,
    outline: Hsla,
    mark: GlyphMark,
    mark_color: Hsla,
    mark_contrast: Hsla,
}

fn battery_glyph(
    battery: &BatteryInfo,
    pal: Palette,
    glyph_size: GlyphSize,
    background: Hsla,
) -> impl IntoElement {
    let tone = battery.tone();
    let color = tone_color(tone, pal);
    let mark = match battery.status {
        BatteryStatus::Charging | BatteryStatus::ChargingSlow => GlyphMark::Charging,
        BatteryStatus::Error => GlyphMark::Error,
        BatteryStatus::Discharging | BatteryStatus::Full | BatteryStatus::Unknown => {
            GlyphMark::None
        }
    };
    let paint = GlyphPaint {
        percentage: battery.bar_percentage(),
        track: pal.muted,
        fill: color,
        outline: pal.text_muted.opacity(0.78),
        mark,
        mark_color: color,
        mark_contrast: background,
    };
    let (width, height) = glyph_size.dimensions();
    canvas(
        move |_, _, _| paint,
        move |bounds, paint, window, _| paint_battery(bounds, paint, window),
    )
    .flex_none()
    .w(px(width))
    .h(px(height))
}

fn paint_battery(bounds: Bounds<gpui::Pixels>, paint: GlyphPaint, window: &mut Window) {
    let width = f32::from(bounds.size.width);
    let height = f32::from(bounds.size.height);
    let large = height > 14.;
    let cap_width = if large { 3. } else { 2. };
    let body_height = height - 2.;
    let body_width = width - cap_width;
    let body_x = f32::from(bounds.origin.x);
    let body_y = f32::from(bounds.origin.y) + 1.;
    let body = Bounds {
        origin: point(px(body_x), px(body_y)),
        size: size(px(body_width), px(body_height)),
    };
    let terminal_height = body_height * 0.38;
    let terminal = Bounds {
        origin: point(
            px(body_x + body_width - 0.5),
            px(body_y + (body_height - terminal_height) / 2.),
        ),
        size: size(px(cap_width), px(terminal_height)),
    };
    window.paint_quad(quad(
        terminal,
        px(cap_width / 2.),
        paint.outline,
        px(0.),
        paint.outline,
        BorderStyle::default(),
    ));

    let radius = px(if large { 4. } else { 3. });
    window.paint_quad(quad(
        body,
        radius,
        paint.track,
        px(1.),
        paint.outline,
        BorderStyle::default(),
    ));

    if let Some(percentage) = paint.percentage.filter(|percentage| *percentage > 0) {
        let inset = 2.;
        let inner_width = body_width - inset * 2.;
        let inner_height = body_height - inset * 2.;
        let fill_width = (inner_width * f32::from(percentage.min(100)) / 100.).max(1.);
        let fill_bounds = Bounds {
            origin: point(px(body_x + inset), px(body_y + inset)),
            size: size(px(fill_width), px(inner_height)),
        };
        let fill_radius: f32 = if large { 2.5 } else { 1.5 };
        let left_radius = px(fill_radius.min(fill_width / 2.));
        let right_radius = px(if percentage >= 96 { fill_radius } else { 0.5 });
        window.paint_quad(quad(
            fill_bounds,
            Corners {
                top_left: left_radius,
                top_right: right_radius,
                bottom_right: right_radius,
                bottom_left: left_radius,
            },
            paint.fill,
            px(0.),
            paint.fill,
            BorderStyle::default(),
        ));
    }

    match paint.mark {
        GlyphMark::None => {}
        GlyphMark::Charging => {
            paint_charging_mark(body, mark_color(paint), window);
        }
        GlyphMark::Error => {
            paint_error_mark(body, mark_color(paint), window);
        }
    }
}

fn mark_color(paint: GlyphPaint) -> Hsla {
    if paint.percentage.is_some_and(|percentage| percentage >= 55) {
        paint.mark_contrast
    } else {
        paint.mark_color
    }
}

fn paint_charging_mark(body: Bounds<gpui::Pixels>, color: Hsla, window: &mut Window) {
    let center_x = f32::from(body.origin.x) + f32::from(body.size.width) / 2.;
    let center_y = f32::from(body.origin.y) + f32::from(body.size.height) / 2.;
    let mark_height = f32::from(body.size.height) * 0.76;
    let mark_width = mark_height * 0.54;
    let mut path = PathBuilder::fill();
    path.add_polygon(
        &[
            point(
                px(center_x + mark_width * 0.12),
                px(center_y - mark_height / 2.),
            ),
            point(
                px(center_x - mark_width * 0.42),
                px(center_y + mark_height * 0.05),
            ),
            point(
                px(center_x - mark_width * 0.06),
                px(center_y + mark_height * 0.05),
            ),
            point(
                px(center_x - mark_width * 0.18),
                px(center_y + mark_height / 2.),
            ),
            point(
                px(center_x + mark_width * 0.45),
                px(center_y - mark_height * 0.12),
            ),
            point(
                px(center_x + mark_width * 0.08),
                px(center_y - mark_height * 0.12),
            ),
        ],
        true,
    );
    if let Ok(path) = path.build() {
        window.paint_path(path, color);
    }
}

fn paint_error_mark(body: Bounds<gpui::Pixels>, color: Hsla, window: &mut Window) {
    let center_x = f32::from(body.origin.x) + f32::from(body.size.width) / 2.;
    let center_y = f32::from(body.origin.y) + f32::from(body.size.height) / 2.;
    let height = f32::from(body.size.height);
    let stroke = (height * 0.13).max(1.);
    let line_height = height * 0.34;
    window.paint_quad(quad(
        Bounds {
            origin: point(px(center_x - stroke / 2.), px(center_y - height * 0.32)),
            size: size(px(stroke), px(line_height)),
        },
        px(stroke / 2.),
        color,
        px(0.),
        color,
        BorderStyle::default(),
    ));
    window.paint_quad(quad(
        Bounds {
            origin: point(px(center_x - stroke / 2.), px(center_y + height * 0.2)),
            size: size(px(stroke), px(stroke)),
        },
        px(stroke / 2.),
        color,
        px(0.),
        color,
        BorderStyle::default(),
    ));
}

#[cfg(test)]
mod tests {
    use gpui::InteractiveElement as _;
    use openlogi_core::device::{BatteryLevel, BatteryReading};

    use super::*;

    /// Title-bar readouts at a one-, two- and three-digit charge, each in a
    /// box as wide as its content.
    struct InlineReadouts;

    impl gpui::Render for InlineReadouts {
        fn render(&mut self, _: &mut Window, _: &mut gpui::Context<Self>) -> impl IntoElement {
            gpui_kit::component::v_flex()
                .items_start()
                .children([9, 74, 100].map(|percentage| {
                    gpui::div()
                        .debug_selector(move || format!("inline-{percentage}"))
                        .child(BatteryIndicator::inline(&BatteryInfo {
                            percentage: Some(percentage),
                            level: BatteryLevel::Good,
                            status: BatteryStatus::Discharging,
                        }))
                }))
        }
    }

    /// A falling charge must not resize the title-bar readout, or the glyph
    /// beside it jumps whenever the digit count changes.
    #[gpui::test]
    fn the_inline_readout_keeps_its_width_as_the_charge_falls(cx: &mut gpui::TestAppContext) {
        cx.update(gpui_kit::init);
        let (_, cx) = cx.add_window_view(|_, _| InlineReadouts);
        cx.update(|window, cx| window.draw(cx).clear(cx));
        let width = |cx: &mut gpui::VisualTestContext, selector| {
            cx.debug_bounds(selector)
                .expect("the readout renders")
                .size
                .width
        };
        let full = width(cx, "inline-100");
        assert_eq!(width(cx, "inline-74"), full);
        assert_eq!(width(cx, "inline-9"), full);
    }

    #[test]
    fn tone_respects_status_priority_and_discharge_thresholds() {
        let cases = [
            (
                5,
                BatteryLevel::Critical,
                BatteryStatus::Charging,
                BatteryTone::Charging,
            ),
            (
                80,
                BatteryLevel::Good,
                BatteryStatus::Error,
                BatteryTone::Error,
            ),
            (
                5,
                BatteryLevel::Critical,
                BatteryStatus::Full,
                BatteryTone::Full,
            ),
            (
                8,
                BatteryLevel::Critical,
                BatteryStatus::Discharging,
                BatteryTone::Critical,
            ),
            (
                20,
                BatteryLevel::Low,
                BatteryStatus::Discharging,
                BatteryTone::Low,
            ),
            (
                21,
                BatteryLevel::Low,
                BatteryStatus::Discharging,
                BatteryTone::Normal,
            ),
        ];
        for (percentage, level, status, expected) in cases {
            let battery = BatteryInfo {
                percentage: Some(percentage),
                level,
                status,
            };
            assert_eq!(battery.tone(), expected);
        }
    }

    fn level_only(level: BatteryLevel, status: BatteryStatus) -> BatteryInfo {
        BatteryInfo {
            percentage: None,
            level,
            status,
        }
    }

    #[test]
    fn a_reading_without_a_percentage_falls_back_to_the_level() {
        let cases = [
            (
                Some(0),
                BatteryLevel::Good,
                BatteryStatus::Charging,
                BatteryReading::NoReading,
            ),
            (
                Some(0),
                BatteryLevel::Critical,
                BatteryStatus::Discharging,
                BatteryReading::Percent(0),
            ),
            (
                Some(64),
                BatteryLevel::Good,
                BatteryStatus::Discharging,
                BatteryReading::Percent(64),
            ),
            (
                None,
                BatteryLevel::Good,
                BatteryStatus::Discharging,
                BatteryReading::Level(BatteryLevel::Good),
            ),
            (
                None,
                BatteryLevel::Low,
                BatteryStatus::Charging,
                BatteryReading::Level(BatteryLevel::Low),
            ),
            (
                None,
                BatteryLevel::Unknown,
                BatteryStatus::Charging,
                BatteryReading::NoReading,
            ),
            (
                None,
                BatteryLevel::Unknown,
                BatteryStatus::Unknown,
                BatteryReading::NoReading,
            ),
        ];
        for (percentage, level, status, expected) in cases {
            let battery = BatteryInfo {
                percentage,
                level,
                status,
            };
            assert_eq!(battery.reading(), expected, "{battery:?}");
        }
    }

    #[test]
    fn a_level_only_battery_is_toned_by_its_level() {
        assert_eq!(
            level_only(BatteryLevel::Critical, BatteryStatus::Charging).tone(),
            BatteryTone::Charging
        );
        assert_eq!(
            level_only(BatteryLevel::Critical, BatteryStatus::Discharging).tone(),
            BatteryTone::Critical
        );
        assert_eq!(
            level_only(BatteryLevel::Low, BatteryStatus::Discharging).tone(),
            BatteryTone::Low
        );
        assert_eq!(
            level_only(BatteryLevel::Good, BatteryStatus::Discharging).tone(),
            BatteryTone::Normal
        );
    }
}
