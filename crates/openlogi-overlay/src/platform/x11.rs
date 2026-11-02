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

use gpui::{Bounds, Pixels, Point, point, px};
use raw_window_handle::RawWindowHandle;
use tracing::debug;
use x11rb::connection::Connection as _;
use x11rb::protocol::shape::{ConnectionExt as _, SK, SO};
use x11rb::protocol::xproto::{
    ClientMessageEvent, ClipOrdering, ConfigureWindowAux, ConnectionExt as _, EventMask, Rectangle,
    Window,
};
use x11rb::rust_connection::RustConnection;

pub(crate) struct CardWindow {
    connection: RustConnection,
    window: Window,
    root: Window,
    override_redirect: bool,
    state_atom: u32,
    above_atom: u32,
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
        let (connection, screen) = match x11rb::connect(None) {
            Ok(connection) => connection,
            Err(error) => {
                debug!(%error, "X11: could not open a connection for the card");
                return None;
            }
        };
        let root = connection
            .setup()
            .roots
            .get(screen)
            .map_or(0, |screen| screen.root);
        let state_atom = connection
            .intern_atom(false, b"_NET_WM_STATE")
            .ok()
            .and_then(|cookie| cookie.reply().ok())
            .map_or(0, |reply| reply.atom);
        let above_atom = connection
            .intern_atom(false, b"_NET_WM_STATE_ABOVE")
            .ok()
            .and_then(|cookie| cookie.reply().ok())
            .map_or(0, |reply| reply.atom);
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
            root,
            override_redirect: attributes.override_redirect,
            state_atom,
            above_atom,
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

    /// Ask the window manager to keep the card above other windows, or stop.
    ///
    /// The card is a managed window in every mode now, so the EWMH
    /// `_NET_WM_STATE_ABOVE` state is the live switch — changing the window
    /// kind (`Normal` ↔ `PopUp`) would destroy and recreate the card for a
    /// stacking change the window manager can make in place.
    pub(crate) fn set_above(&self, above: bool) {
        if self.override_redirect || self.state_atom == 0 || self.above_atom == 0 {
            return;
        }
        // `_NET_WM_STATE`: 1 adds the listed state, 0 removes it; the second
        // word is the state, and 1 says a normal application asked.
        let event = ClientMessageEvent::new(
            32,
            self.window,
            self.state_atom,
            [u32::from(above), self.above_atom, 0, 1, 0],
        );
        let sent: anyhow::Result<()> = (|| {
            self.connection
                .send_event(
                    false,
                    self.root,
                    EventMask::SUBSTRUCTURE_REDIRECT | EventMask::SUBSTRUCTURE_NOTIFY,
                    event,
                )?
                .check()?;
            self.connection.flush()?;
            Ok(())
        })();
        if let Err(error) = sent {
            debug!(%error, "X11: could not change the card's stacking");
        }
    }

    /// Keep pointer input inside the pin button while the card is pinned, or
    /// restore the whole window when it is not.
    ///
    /// X11 has no per-window click-through bit; the Shape extension's input
    /// shape is the native switch. A null source mask restores the default
    /// whole-window shape. `pin` is in the card's logical window coordinates.
    pub(crate) fn set_click_through(&self, pinned: bool, scale: f32, pin: Bounds<Pixels>) {
        let shaped: anyhow::Result<()> = (|| {
            if pinned {
                let (x, y, width, height) = physical_rect(pin, scale);
                self.connection
                    .shape_rectangles(
                        SO::SET,
                        SK::INPUT,
                        ClipOrdering::UNSORTED,
                        self.window,
                        0,
                        0,
                        &[Rectangle {
                            x,
                            y,
                            width,
                            height,
                        }],
                    )?
                    .check()?;
            } else {
                self.connection
                    .shape_mask(SO::SET, SK::INPUT, self.window, 0, 0, x11rb::NONE)?
                    .check()?;
            }
            self.connection.flush()?;
            Ok(())
        })();
        if let Err(error) = shaped {
            debug!(%error, "X11: could not set the card's input shape");
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

/// `bounds` in the device pixels X11's Shape extension expects.
#[expect(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "the pin rectangle is a 20 px layout constant scaled onto a screen-sized card, inside i16/u16"
)]
fn physical_rect(bounds: Bounds<Pixels>, scale: f32) -> (i16, i16, u16, u16) {
    let x = (f32::from(bounds.origin.x) * scale).round();
    let y = (f32::from(bounds.origin.y) * scale).round();
    let width = (f32::from(bounds.size.width) * scale).round();
    let height = (f32::from(bounds.size.height) * scale).round();
    (x as i16, y as i16, width as u16, height as u16)
}

#[cfg(test)]
mod tests {
    use super::{physical, physical_rect};
    use gpui::{Bounds, point, px, size};

    #[test]
    fn a_logical_origin_becomes_device_pixels() {
        assert_eq!(physical(point(px(12.4), px(7.6)), 2.0), (25, 15));
        assert_eq!(physical(point(px(-3.0), px(4.25)), 1.25), (-4, 5));
    }

    #[test]
    fn a_logical_pin_rectangle_becomes_device_pixels() {
        let pin = Bounds::new(point(px(264.0), px(14.0)), size(px(20.0), px(20.0)));
        assert_eq!(physical_rect(pin, 1.0), (264, 14, 20, 20));
        assert_eq!(physical_rect(pin, 2.0), (528, 28, 40, 40));
    }
}
