//! Windows placement stays in physical virtual-desktop coordinates. GPUI's
//! per-monitor logical rectangles can overlap at mixed DPI, so neither cursor
//! monitor selection nor HWND positioning can use logical containment.

use gpui::{
    Bounds, DevicePixels, DisplayId, Pixels, Point, Size, WindowBounds, WindowOptions, point, size,
};

use crate::flow_indicator::MARGIN;
use crate::notice::{BOTTOM_MARGIN, notice_window_options};
use crate::ring::{Placement, WINDOW_SIZE, ring_window_options};

#[cfg(target_os = "windows")]
mod native;

/// One active display in physical virtual-desktop pixels.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct MonitorInfo {
    /// The GDI device name (`\\.\DISPLAY1`), stable across reconnects.
    pub(crate) device: String,
    pub(crate) display_id: DisplayId,
    /// The whole monitor (`rcMonitor`).
    pub(crate) monitor: Bounds<DevicePixels>,
    /// The monitor without the taskbar (`rcWork`).
    pub(crate) work: Bounds<DevicePixels>,
    pub(crate) dpi: u32,
}

/// Whether the display topology changed between two monitor layouts.
///
/// The work area is deliberately left out: the taskbar coming and going must
/// not re-anchor the card or pause its position writes. A resolution, DPI, or
/// monitor-set change is a topology change.
pub(crate) fn topology_changed(previous: Option<&[MonitorInfo]>, current: &[MonitorInfo]) -> bool {
    let Some(previous) = previous else {
        return true;
    };
    previous.len() != current.len()
        || previous.iter().zip(current).any(|(before, after)| {
            before.device != after.device
                || before.monitor != after.monitor
                || before.dpi != after.dpi
        })
}

/// A physical place for the visible card.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct CardTarget {
    pub(crate) origin: Point<DevicePixels>,
    pub(crate) display_id: DisplayId,
    /// The target monitor's `dpi / 96`, for converting the card's logical size.
    pub(crate) scale: f32,
}

/// Where the visible card belongs, in physical pixels.
///
/// A saved point stays on its own monitor while that monitor exists; a point
/// whose monitor is gone falls back to the monitor that contains it, then to
/// the primary. The card is clamped into the chosen work area so a removed or
/// resized display cannot strand it off-screen.
#[expect(
    clippy::cast_possible_truncation,
    reason = "a display's DPI divided by 96 and a screen-sized position fit their wider types"
)]
pub(crate) fn card_target(
    layout: &[MonitorInfo],
    saved: Option<(Point<DevicePixels>, Option<&str>)>,
    card: Size<Pixels>,
    scale: Option<f32>,
) -> Option<CardTarget> {
    let primary = layout
        .iter()
        .find(|monitor| monitor.monitor.origin == point(DevicePixels(0), DevicePixels(0)))
        .or_else(|| layout.first())?;
    let monitor = saved
        .and_then(|(_, device)| {
            device.and_then(|device| layout.iter().find(|monitor| monitor.device == device))
        })
        .or_else(|| {
            saved.and_then(|(origin, _)| {
                layout
                    .iter()
                    .find(|monitor| monitor.monitor.contains(&origin))
            })
        })
        .unwrap_or(primary);
    // A virtual display can report a different effective DPI to
    // `GetDpiForMonitor` than the DPI the window actually gets. The window's
    // own scale is the one its card is painted at, so callers that have a
    // window pass it in; only a not-yet-created window falls back to the
    // monitor's DPI.
    let scale = scale.map_or_else(|| f64::from(monitor.dpi) / 96.0, f64::from);
    let visible = size(
        DevicePixels((f64::from(f32::from(card.width)) * scale).round() as i32),
        DevicePixels((f64::from(f32::from(card.height)) * scale).round() as i32),
    );
    let margin = (f64::from(MARGIN) * scale).round() as i32;
    let desired = saved.map_or_else(
        || {
            point(
                monitor.work.right() - visible.width - DevicePixels(margin),
                monitor.work.bottom() - visible.height - DevicePixels(margin),
            )
        },
        |(origin, _)| origin,
    );
    let max = point(
        (monitor.work.right() - visible.width).max(monitor.work.left()),
        (monitor.work.bottom() - visible.height).max(monitor.work.top()),
    );
    Some(CardTarget {
        origin: desired.clamp(&monitor.work.origin, &max),
        display_id: monitor.display_id,
        scale: scale as f32,
    })
}

#[cfg(target_os = "windows")]
pub(crate) use native::{
    card_dpi_mismatch, card_needs_raise, card_physical_placement, monitor_layout,
    place_card_visible, set_card_layout_scale, take_card_user_moved,
};

/// Show or hide the card's window without destroying it; see
/// [`crate::platform::hide_card`].
#[cfg(target_os = "windows")]
pub(crate) use native::set_card_visible;

/// Remove the Win32 frame from the card's window; see
/// [`native::make_card_frameless`].
#[cfg(target_os = "windows")]
pub(crate) use native::make_card_frameless;

/// Keep the card's window from taking the foreground when clicked; see
/// [`native::make_card_nonactivating`].
#[cfg(target_os = "windows")]
pub(crate) use native::make_card_nonactivating;

/// Move the card without resizing or activating it; see
/// [`native::move_card_by`].
#[cfg(target_os = "windows")]
pub(crate) use native::move_card_by;

/// Watch the pinned card's click-through switch; see
/// [`native::set_card_click_through`].
#[cfg(target_os = "windows")]
pub(crate) use native::set_card_click_through;

/// One native cursor/monitor snapshot, kept through bootstrap-window creation.
pub(crate) struct RingPlacement {
    display_id: DisplayId,
    cursor: Point<DevicePixels>,
    display: Bounds<DevicePixels>,
    /// The selected monitor's effective DPI, used only to express the
    /// bootstrap bounds in that monitor's logical pixels.
    dpi: u32,
}

impl RingPlacement {
    /// The one set of windows to try: the window at the cursor, paired with
    /// how its view places the panel.
    pub(crate) fn openings(&self) -> Vec<Vec<(WindowOptions, Placement)>> {
        vec![vec![(self.window_options(), Placement::Window)]]
    }

    pub(crate) fn window_options(&self) -> WindowOptions {
        WindowOptions {
            display_id: Some(self.display_id),
            // GPUI's Windows backend only sets up DWM transparency for a window
            // created visible, so the ring opens shown — as a 1px bootstrap at
            // the selected monitor's centre until `native.rs` places it.
            window_bounds: Some(WindowBounds::Windowed(self.bootstrap_bounds())),
            show: true,
            ..ring_window_options()
        }
    }

    /// The 1px bootstrap at the monitor's centre, in its logical pixels.
    fn bootstrap_bounds(&self) -> Bounds<Pixels> {
        bootstrap_bounds(self.display, self.dpi)
    }

    /// GPUI's per-monitor scale factor, `dpi / 96`.
    #[cfg(test)]
    fn scale_factor(&self) -> f32 {
        scale_factor(self.dpi)
    }

    #[expect(
        clippy::cast_possible_truncation,
        reason = "360 DIP scaled by a native display DPI fits in screen-sized i32 pixels"
    )]
    fn bounds(&self, dpi: u32) -> Bounds<DevicePixels> {
        let edge = DevicePixels((f64::from(WINDOW_SIZE) * f64::from(dpi) / 96.0).round() as i32);
        let half = DevicePixels(edge.0 / 2);
        let desired = self.cursor - point(half, half);
        // If the display is smaller than the ring, anchor to its top-left
        // rather than inverting the clamp range or changing the ring's scale.
        let max = point(
            (self.display.right() - edge).max(self.display.left()),
            (self.display.bottom() - edge).max(self.display.top()),
        );
        Bounds::new(desired.clamp(&self.display.origin, &max), size(edge, edge))
    }
}

/// One native monitor snapshot for the notice window, kept through
/// bootstrap-window creation.
pub(crate) struct NoticePlacement {
    display_id: DisplayId,
    display: Bounds<DevicePixels>,
    /// The monitor without the taskbar (`rcWork`), where the notice sits.
    work: Bounds<DevicePixels>,
    /// The selected monitor's effective DPI, as for the ring.
    dpi: u32,
    /// The notice window's size in logical pixels.
    size: Size<Pixels>,
}

impl NoticePlacement {
    /// The notice window, opened as the same 1px bootstrap as the ring.
    pub(crate) fn openings(&self) -> Vec<WindowOptions> {
        vec![WindowOptions {
            display_id: Some(self.display_id),
            window_bounds: Some(WindowBounds::Windowed(bootstrap_bounds(
                self.display,
                self.dpi,
            ))),
            show: true,
            ..notice_window_options()
        }]
    }

    /// The notice at the bottom centre of the work area, above the taskbar.
    #[expect(
        clippy::cast_possible_truncation,
        reason = "notice dimensions scaled by a native display DPI fit in screen-sized i32 pixels"
    )]
    fn bounds(&self, dpi: u32) -> Bounds<DevicePixels> {
        let scaled = |logical: f32| {
            DevicePixels((f64::from(logical) * f64::from(dpi) / 96.0).round() as i32)
        };
        let width = scaled(self.size.width.as_f32());
        let height = scaled(self.size.height.as_f32());
        let work = self.work;
        let desired = point(
            work.left() + DevicePixels((work.size.width.0 - width.0) / 2),
            work.bottom() - height - scaled(BOTTOM_MARGIN),
        );
        // A work area smaller than the notice anchors it to the top-left, as
        // an undersized display does for the ring.
        let max = point(
            (work.right() - width).max(work.left()),
            (work.bottom() - height).max(work.top()),
        );
        Bounds::new(desired.clamp(&work.origin, &max), size(width, height))
    }
}

/// The 1px bootstrap at the monitor's centre, in its logical pixels.
///
/// GPUI validates requested bounds by scaling their centre with the requested
/// monitor's own factor. Bounds that land elsewhere are replaced by its
/// default (large, and here visible) window, so they must be expressed for
/// this monitor rather than as a global logical point.
fn bootstrap_bounds(display: Bounds<DevicePixels>, dpi: u32) -> Bounds<Pixels> {
    Bounds::new(display.center(), size(DevicePixels(1), DevicePixels(1)))
        .to_pixels(scale_factor(dpi))
}

/// GPUI's per-monitor scale factor, `dpi / 96`.
#[expect(
    clippy::cast_precision_loss,
    reason = "effective DPIs are a few hundred, exactly representable in f32"
)]
fn scale_factor(dpi: u32) -> f32 {
    dpi as f32 / 96.0
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::notice::stack_size;

    fn placement(cursor: (i32, i32), rect: (i32, i32, i32, i32)) -> RingPlacement {
        RingPlacement {
            display_id: DisplayId::from(42),
            cursor: point(DevicePixels(cursor.0), DevicePixels(cursor.1)),
            display: Bounds::new(
                point(DevicePixels(rect.0), DevicePixels(rect.1)),
                size(DevicePixels(rect.2 - rect.0), DevicePixels(rect.3 - rect.1)),
            ),
            dpi: 96,
        }
    }

    /// The single positioned window this placement opens.
    #[track_caller]
    fn opened(placement: &RingPlacement) -> WindowOptions {
        let mut windows = placement.openings().into_iter().flatten();
        let (options, panel) = windows.next().expect("a Windows ring opens a window");
        assert!(windows.next().is_none(), "a Windows ring opens one window");
        assert_eq!(panel, Placement::Window);
        options
    }

    /// Mirror GPUI's validation: the requested bounds, scaled by the requested
    /// monitor's own factor, must land back on that monitor as a 1px window.
    #[track_caller]
    fn assert_bootstrap_on_display(placement: &RingPlacement) {
        let Some(WindowBounds::Windowed(bounds)) = opened(placement).window_bounds else {
            panic!("the ring must request windowed bootstrap bounds");
        };
        let device = bounds.to_device_pixels(placement.scale_factor());
        assert!(
            placement.display.contains(&device.center()),
            "bootstrap {device:?} is off its monitor {:?}",
            placement.display
        );
        assert_eq!(device.size, size(DevicePixels(1), DevicePixels(1)));
    }

    #[test]
    fn native_monitor_identity_survives_logical_overlap() {
        // 100% primary [0,1920), 200% right-hand monitor [1920,4480). The
        // bootstrap's logical centre (1600,360) also lies inside the primary's
        // logical rectangle, yet must resolve to the 200% monitor.
        let placement = RingPlacement {
            dpi: 192,
            ..placement((2000, 800), (1920, 0, 4480, 1440))
        };
        let options = opened(&placement);
        assert_eq!(options.display_id, Some(DisplayId::from(42)));
        assert!(
            options.show,
            "GPUI only applies DWM transparency to a window created visible"
        );
        assert_bootstrap_on_display(&placement);
        let bounds = placement.bounds(192);
        assert_eq!(bounds.origin, point(DevicePixels(1920), DevicePixels(440)));
        assert_eq!(bounds.size, size(DevicePixels(720), DevicePixels(720)));
    }

    #[test]
    fn bootstrap_opens_on_each_mixed_dpi_monitor() {
        // A 150% primary at the origin and a 125% monitor to its left: a
        // global (0,0) bootstrap matched only the primary, so on the left
        // monitor GPUI fell back to its large default window instead.
        for (cursor, rect, dpi) in [
            ((700, 1200), (0, 0, 1440, 2560), 144),
            ((-1200, 800), (-2560, 105, 0, 1545), 120),
        ] {
            assert_bootstrap_on_display(&RingPlacement {
                dpi,
                ..placement(cursor, rect)
            });
        }
    }

    #[test]
    fn physical_center_is_independent_of_monitor_scale_and_global_origin() {
        for (cursor, rect, dpi, origin, edge) in [
            ((3000, 850), (1920, 0, 4480, 1440), 192, (2640, 490), 720),
            ((900, 600), (0, 0, 1920, 1080), 96, (720, 420), 360),
            // 150% left, 125% above, and 175% below-left with nonzero X/Y.
            ((-1400, 450), (-2560, -200, 0, 1240), 144, (-1670, 180), 540),
            ((-300, -700), (-640, -1440, 1920, 0), 120, (-525, -925), 450),
            (
                (-1200, 1600),
                (-1920, 1080, 0, 2520),
                168,
                (-1515, 1285),
                630,
            ),
        ] {
            let bounds = placement(cursor, rect).bounds(dpi);
            assert_eq!(
                bounds.origin,
                point(DevicePixels(origin.0), DevicePixels(origin.1))
            );
            assert_eq!(bounds.size, size(DevicePixels(edge), DevicePixels(edge)));
        }
    }

    #[test]
    fn monitor_seams_and_outer_edges_clamp_on_the_selected_side() {
        for (cursor, rect, dpi, origin) in [
            ((1919, 600), (0, 0, 1920, 1080), 96, (1560, 420)),
            ((1920, 600), (1920, 0, 4480, 1440), 192, (1920, 240)),
            ((4479, 1439), (1920, 0, 4480, 1440), 192, (3760, 720)),
            ((-1, 600), (-2560, -200, 0, 1240), 144, (-540, 330)),
            ((0, 600), (0, 0, 1920, 1080), 96, (0, 420)),
            ((600, -1), (-640, -1440, 1920, 0), 120, (375, -450)),
            ((600, 0), (0, 0, 1920, 1080), 96, (420, 0)),
            ((-640, -1440), (-640, -1440, 1920, 0), 120, (-640, -1440)),
        ] {
            let bounds = placement(cursor, rect).bounds(dpi);
            assert_eq!(
                bounds.origin,
                point(DevicePixels(origin.0), DevicePixels(origin.1))
            );
        }
    }

    #[test]
    fn undersized_display_keeps_a_valid_clamp_range() {
        let bounds = placement((-100, -50), (-200, -100, 0, 0)).bounds(192);
        assert_eq!(bounds.origin, point(DevicePixels(-200), DevicePixels(-100)));
        assert_eq!(bounds.size, size(DevicePixels(720), DevicePixels(720)));
    }

    fn rect_bounds(rect: (i32, i32, i32, i32)) -> Bounds<DevicePixels> {
        Bounds::new(
            point(DevicePixels(rect.0), DevicePixels(rect.1)),
            size(DevicePixels(rect.2 - rect.0), DevicePixels(rect.3 - rect.1)),
        )
    }

    fn notice(
        display: (i32, i32, i32, i32),
        work: (i32, i32, i32, i32),
        dpi: u32,
    ) -> NoticePlacement {
        NoticePlacement {
            display_id: DisplayId::from(7),
            display: rect_bounds(display),
            work: rect_bounds(work),
            dpi,
            // One card: 344×88 logical pixels.
            size: stack_size(1),
        }
    }

    #[test]
    fn a_notice_sits_centred_above_the_taskbar() {
        // 1920×1080 at 100%, with a 48px taskbar along the bottom.
        let bounds = notice((0, 0, 1920, 1080), (0, 0, 1920, 1032), 96).bounds(96);
        assert_eq!(bounds.size, size(DevicePixels(344), DevicePixels(88)));
        // Centred: (1920 - 344) / 2 = 788. Above the taskbar by the margin:
        // 1032 - 88 - 48 = 896.
        assert_eq!(bounds.origin, point(DevicePixels(788), DevicePixels(896)));
    }

    #[test]
    fn a_notice_scales_with_its_monitor_and_follows_its_work_area() {
        // A 200% monitor right of the primary, taskbar on its left edge.
        let bounds = notice((1920, 0, 4480, 1440), (2016, 0, 4480, 1440), 192).bounds(192);
        assert_eq!(bounds.size, size(DevicePixels(688), DevicePixels(176)));
        // (2464 - 688) / 2 = 888 into the work area; 1440 - 176 - 96 = 1168.
        assert_eq!(bounds.origin, point(DevicePixels(2904), DevicePixels(1168)));
    }

    #[test]
    fn a_notice_opens_as_a_bootstrap_on_its_monitor() {
        let placement = notice((-2560, 105, 0, 1545), (-2560, 105, 0, 1497), 120);
        let options = placement
            .openings()
            .into_iter()
            .next()
            .expect("a Windows notice opens one window");
        assert_eq!(options.display_id, Some(DisplayId::from(7)));
        assert!(
            options.show,
            "GPUI only applies DWM transparency to a window created visible"
        );
        let Some(WindowBounds::Windowed(bounds)) = options.window_bounds else {
            panic!("the notice must request windowed bootstrap bounds");
        };
        let device = bounds.to_device_pixels(scale_factor(120));
        assert!(placement.display.contains(&device.center()));
        assert_eq!(device.size, size(DevicePixels(1), DevicePixels(1)));
    }

    #[test]
    fn a_work_area_smaller_than_the_notice_anchors_it_top_left() {
        let bounds = notice((0, 0, 200, 60), (0, 0, 200, 60), 96).bounds(96);
        assert_eq!(bounds.origin, point(DevicePixels(0), DevicePixels(0)));
    }

    fn monitor(
        device: &str,
        display_id: u64,
        rect: (i32, i32, i32, i32),
        work: (i32, i32, i32, i32),
        dpi: u32,
    ) -> MonitorInfo {
        MonitorInfo {
            device: device.to_string(),
            display_id: DisplayId::from(display_id),
            monitor: rect_bounds(rect),
            work: rect_bounds(work),
            dpi,
        }
    }

    fn card() -> Size<gpui::Pixels> {
        gpui::size(gpui::px(300.0), gpui::px(211.0))
    }

    #[test]
    fn a_saved_card_returns_to_its_own_monitor() {
        let layout = [
            monitor(
                r"\\.\DISPLAY1",
                1,
                (0, 0, 2560, 1440),
                (0, 0, 2560, 1380),
                120,
            ),
            monitor(
                r"\\.\DISPLAY2",
                2,
                (2560, 0, 4480, 1080),
                (2560, 0, 4480, 1080),
                96,
            ),
        ];

        let target = card_target(
            &layout,
            Some((
                point(DevicePixels(3000), DevicePixels(500)),
                Some(r"\\.\DISPLAY2"),
            )),
            card(),
            None,
        )
        .expect("a target");

        assert_eq!(target.origin, point(DevicePixels(3000), DevicePixels(500)));
        assert_eq!(target.display_id, DisplayId::from(2));
        assert!((target.scale - 1.0).abs() < f32::EPSILON);
    }

    #[test]
    fn a_missing_display_clamps_the_card_into_the_primary() {
        let layout = [monitor(
            r"\\.\DISPLAY1",
            1,
            (0, 0, 2560, 1440),
            (0, 0, 2560, 1380),
            120,
        )];

        let target = card_target(
            &layout,
            Some((
                point(DevicePixels(3000), DevicePixels(500)),
                Some(r"\\.\DISPLAY2"),
            )),
            card(),
            None,
        )
        .expect("a target");

        assert_eq!(
            target.origin,
            point(DevicePixels(2560 - 375), DevicePixels(500)),
            "a point from the gone display is clamped into the primary at its size"
        );
        assert_eq!(target.display_id, DisplayId::from(1));
    }

    #[test]
    fn a_resized_display_keeps_the_card_inside() {
        let layout = [monitor(
            r"\\.\DISPLAY1",
            1,
            (0, 0, 1920, 1080),
            (0, 0, 1920, 1080),
            96,
        )];

        let target = card_target(
            &layout,
            Some((
                point(DevicePixels(1800), DevicePixels(1000)),
                Some(r"\\.\DISPLAY1"),
            )),
            card(),
            None,
        )
        .expect("a target");

        assert_eq!(
            target.origin,
            point(DevicePixels(1920 - 300), DevicePixels(1080 - 211))
        );
    }

    #[test]
    fn a_card_without_a_place_opens_bottom_right() {
        let layout = [monitor(
            r"\\.\DISPLAY1",
            1,
            (0, 0, 1920, 1080),
            (0, 0, 1920, 1032),
            96,
        )];

        let target = card_target(&layout, None, card(), None).expect("a target");

        assert_eq!(
            target.origin,
            point(DevicePixels(1920 - 300 - 24), DevicePixels(1032 - 211 - 24))
        );
    }

    #[test]
    fn a_card_is_sized_at_the_target_scale() {
        let layout = [monitor(
            r"\\.\DISPLAY1",
            1,
            (0, 0, 3840, 2160),
            (0, 0, 3840, 2160),
            192,
        )];

        let target = card_target(&layout, None, card(), None).expect("a target");

        assert_eq!(
            target.origin,
            point(DevicePixels(3840 - 600 - 48), DevicePixels(2160 - 422 - 48))
        );
        assert!((target.scale - 2.0).abs() < f32::EPSILON);
    }

    #[test]
    fn a_saved_display_wins_over_the_point_monitor() {
        let layout = [
            monitor(
                r"\\.\DISPLAY1",
                1,
                (0, 0, 1920, 1080),
                (0, 0, 1920, 1080),
                96,
            ),
            monitor(
                r"\\.\DISPLAY2",
                2,
                (1920, 0, 4480, 1440),
                (1920, 0, 4480, 1440),
                120,
            ),
        ];

        let target = card_target(
            &layout,
            Some((
                point(DevicePixels(500), DevicePixels(500)),
                Some(r"\\.\DISPLAY2"),
            )),
            card(),
            None,
        )
        .expect("a target");

        assert_eq!(
            target.display_id,
            DisplayId::from(2),
            "the pinned display owns the card while it exists"
        );
        assert_eq!(target.origin, point(DevicePixels(1920), DevicePixels(500)));
    }

    #[test]
    fn a_card_larger_than_the_work_area_anchors_top_left() {
        let layout = [monitor(
            r"\\.\DISPLAY1",
            1,
            (0, 0, 200, 60),
            (0, 0, 200, 60),
            96,
        )];

        let target = card_target(&layout, None, card(), None).expect("a target");
        assert_eq!(target.origin, point(DevicePixels(0), DevicePixels(0)));
    }

    #[test]
    fn the_card_uses_the_window_scale_not_the_reported_monitor_dpi() {
        // The virtual display reports 120 through GetDpiForMonitor while the
        // window is actually painted at 150%; sizing the clamp from the
        // reported DPI would leave the card short of the screen edge.
        let layout = [monitor(
            r"\\.\DISPLAY56",
            56,
            (0, 0, 1600, 900),
            (0, 0, 1600, 840),
            120,
        )];

        let target = card_target(&layout, None, card(), Some(1.5)).expect("a target");

        assert_eq!(
            target.origin,
            point(DevicePixels(1600 - 450 - 36), DevicePixels(840 - 317 - 36))
        );
        assert!((target.scale - 1.5).abs() < f32::EPSILON);
    }

    #[test]
    fn a_taskbar_change_is_not_a_display_topology_change() {
        let primary = monitor(
            r"\\.\DISPLAY1",
            1,
            (0, 0, 2560, 1440),
            (0, 0, 2560, 1380),
            120,
        );
        let taskbar_moved = monitor(
            r"\\.\DISPLAY1",
            1,
            (0, 0, 2560, 1440),
            (0, 0, 2560, 1440),
            120,
        );
        assert!(!topology_changed(
            Some(std::slice::from_ref(&primary)),
            &[taskbar_moved]
        ));

        let resized = monitor(
            r"\\.\DISPLAY1",
            1,
            (0, 0, 1920, 1080),
            (0, 0, 1920, 1080),
            120,
        );
        assert!(topology_changed(
            Some(std::slice::from_ref(&primary)),
            &[resized]
        ));

        let higher_dpi = monitor(
            r"\\.\DISPLAY1",
            1,
            (0, 0, 2560, 1440),
            (0, 0, 2560, 1380),
            144,
        );
        assert!(topology_changed(Some(&[primary]), &[higher_dpi]));
        assert!(topology_changed(None, &[]));
    }
}
