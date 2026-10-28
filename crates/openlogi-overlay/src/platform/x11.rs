//! The card's own X11 connection: the window moves GPUI cannot make.
//!
//! GPUI creates the card at a requested origin, but a window manager places a
//! managed window itself, and `WindowKind::PopUp` is override-redirect, which
//! no window manager moves. GPUI's only move entry point,
//! `Window::start_window_move`, sends `_NET_WM_MOVERESIZE` with button 0, and
//! xfwm4 drives its move loop from that button number, so the drag never
//! starts. Moving the window from a second connection works for both kinds:
//! a managed window turns the request into the `ConfigureRequest` its window
//! manager applies, an override-redirect window simply moves.

use gpui::{Pixels, Point, point, px};
use raw_window_handle::RawWindowHandle;
use tracing::debug;
use x11rb::connection::Connection as _;
use x11rb::protocol::xproto::{ConfigureWindowAux, ConnectionExt as _, Window};
use x11rb::rust_connection::RustConnection;

pub(crate) struct CardWindow {
    connection: RustConnection,
    window: Window,
    override_redirect: bool,
}

impl CardWindow {
    /// The X11 window behind `window`, or `None` when this is not an X11
    /// session or the connection could not be opened.
    pub(crate) fn new(window: &gpui::Window) -> Option<Self> {
        // The inherent `Window::window_handle` returns GPUI's own handle; the
        // trait method is the native one.
        let handle = raw_window_handle::HasWindowHandle::window_handle(window).ok()?;
        let RawWindowHandle::Xcb(handle) = handle.as_raw() else {
            return None;
        };
        let (connection, _) = match x11rb::connect(None) {
            Ok(connection) => connection,
            Err(error) => {
                debug!(%error, "X11: could not open a connection for the card");
                return None;
            }
        };
        let attributes = match connection
            .get_window_attributes(handle.window.get())
            .ok()?
            .reply()
        {
            Ok(attributes) => attributes,
            Err(error) => {
                debug!(%error, "X11: could not read the card window's attributes");
                return None;
            }
        };
        Some(Self {
            connection,
            window: handle.window.get(),
            override_redirect: attributes.override_redirect,
        })
    }

    /// Whether the window manager leaves this window alone, so a position set
    /// when the window was created is final.
    pub(crate) const fn is_override_redirect(&self) -> bool {
        self.override_redirect
    }

    /// Whether the window still exists. Closing the card from its task-list
    /// button destroys the X11 window, after which the server answers
    /// `BadWindow`.
    pub(crate) fn is_alive(&self) -> bool {
        self.connection
            .get_window_attributes(self.window)
            .ok()
            .and_then(|cookie| cookie.reply().ok())
            .is_some()
    }

    /// The pointer's root position, in the logical pixels GPUI reports.
    ///
    /// A drag cannot accumulate the surface-relative positions GPUI delivers:
    /// the window moves under the pointer while the request is in flight, so
    /// those positions do not describe the pointer's own movement.
    pub(crate) fn pointer_root(&self, scale: f32) -> Option<Point<Pixels>> {
        let reply = self
            .connection
            .query_pointer(self.window)
            .ok()?
            .reply()
            .ok()?;
        Some(point(
            px(f32::from(reply.root_x) / scale),
            px(f32::from(reply.root_y) / scale),
        ))
    }

    /// Put the card's top-left corner at `origin`, in the device pixels X11
    /// places windows in. A managed window's manager is free to adjust the
    /// request; the caller retries until the window is where it asked.
    pub(crate) fn move_to(&self, origin: Point<Pixels>, scale: f32) {
        let (x, y) = physical(origin, scale);
        let moved: anyhow::Result<()> = (|| {
            self.connection
                .configure_window(self.window, &ConfigureWindowAux::new().x(x).y(y))?
                .check()?;
            self.connection.flush()?;
            Ok(())
        })();
        if let Err(error) = moved {
            debug!(%error, "X11: could not move the card");
        }
    }
}

/// `origin` in the device pixels X11 places windows in.
#[expect(
    clippy::cast_possible_truncation,
    reason = "a window origin is a screen-sized logical coordinate times the scale factor, far inside the i32 X11 range"
)]
fn physical(origin: Point<Pixels>, scale: f32) -> (i32, i32) {
    (
        (f32::from(origin.x) * scale).round() as i32,
        (f32::from(origin.y) * scale).round() as i32,
    )
}

#[cfg(test)]
mod tests {
    use super::physical;
    use gpui::{point, px};

    #[test]
    fn a_logical_origin_becomes_device_pixels() {
        assert_eq!(physical(point(px(12.4), px(7.6)), 2.0), (25, 15));
        assert_eq!(physical(point(px(-3.0), px(4.25)), 1.25), (-4, 5));
    }
}
