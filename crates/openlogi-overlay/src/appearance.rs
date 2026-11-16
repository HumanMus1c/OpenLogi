//! The card's light/dark decision.
//!
//! GPUI reads `org.freedesktop.appearance color-scheme` once, when its Linux
//! platform client starts, and never retries. The overlay autostarts with the
//! session, and at login `xdg-desktop-portal` is often D-Bus-activated later
//! (by the first app that needs it), so that one read can fail: every window
//! then reports Light for the rest of the session and no style change is ever
//! observed. This watcher owns the decision for the overlay instead — it
//! retries until the portal answers, then follows its `SettingChanged` signal.
//!
//! Windows and macOS need none of this: their platform window reports
//! appearance changes live, and [`Appearance`] stays unknown there so the card
//! falls back to it.

use std::sync::Arc;
use std::sync::atomic::{AtomicU8, AtomicU64, Ordering};

/// The portal's `color-scheme` values; 0 (no preference) reads as light, the
/// same default GPUI uses.
const UNKNOWN: u8 = u8::MAX;
const LIGHT: u8 = 0;
const DARK: u8 = 1;

#[derive(Debug)]
pub(crate) struct Appearance {
    mode: AtomicU8,
    revision: AtomicU64,
}

impl Appearance {
    /// A mode that is still unknown; tests use it without the watcher.
    pub(crate) fn new() -> Self {
        Self {
            mode: AtomicU8::new(UNKNOWN),
            revision: AtomicU64::new(0),
        }
    }

    /// Start following the desktop's appearance.
    pub(crate) fn start() -> Arc<Self> {
        let appearance = Arc::new(Self::new());
        #[cfg(target_os = "linux")]
        linux::follow(Arc::clone(&appearance));
        appearance
    }

    /// The watched mode, or `None` until the desktop has reported one (always
    /// `None` off Linux, where the window's own appearance is the source).
    pub(crate) fn is_dark(&self) -> Option<bool> {
        match self.mode.load(Ordering::Relaxed) {
            DARK => Some(true),
            LIGHT => Some(false),
            _ => None,
        }
    }

    /// Moves only when the mode flips, so the card's one-second tick can tell
    /// whether it has anything to repaint.
    pub(crate) fn revision(&self) -> u64 {
        self.revision.load(Ordering::Relaxed)
    }

    #[cfg(target_os = "linux")]
    fn set_dark(&self, dark: bool) {
        let mode = if dark { DARK } else { LIGHT };
        if self.mode.swap(mode, Ordering::Relaxed) != mode {
            self.revision.fetch_add(1, Ordering::Relaxed);
        }
    }
}

#[cfg(target_os = "linux")]
mod linux {
    use std::thread;
    use std::time::Duration;

    use tracing::{debug, info, warn};
    use zbus::blocking::Connection;
    use zbus::proxy;
    use zbus::zvariant::{OwnedValue, Value};

    use super::Appearance;

    const NAMESPACE: &str = "org.freedesktop.appearance";
    const COLOR_SCHEME: &str = "color-scheme";
    const RETRY: Duration = Duration::from_secs(2);
    /// How long one connection keeps waiting for the portal to answer before
    /// it is replaced. The portal is D-Bus-activated; at login it can take a
    /// while, and a connection that outlived a portal crash must not be reused.
    const WAIT_ATTEMPTS: u8 = 30;

    #[proxy(
        interface = "org.freedesktop.portal.Settings",
        default_service = "org.freedesktop.portal.Desktop",
        default_path = "/org/freedesktop/portal/desktop",
        gen_blocking = true
    )]
    trait Settings {
        fn read(&self, namespace: &str, key: &str) -> zbus::Result<OwnedValue>;

        #[zbus(signal, name = "SettingChanged")]
        fn setting_changed(
            &self,
            namespace: &str,
            key: &str,
            value: OwnedValue,
        ) -> zbus::Result<()>;
    }

    /// Follow the portal on a process-lifetime thread. Failure is non-fatal:
    /// the card keeps whatever the window reported.
    pub(super) fn follow(appearance: std::sync::Arc<Appearance>) {
        let spawned = thread::Builder::new()
            .name("openlogi-appearance".into())
            .spawn(move || {
                loop {
                    // The portal is D-Bus-activated, so an absent one is the
                    // normal state at login; wait and connect again.
                    if let Err(error) = listen(&appearance) {
                        debug!(%error, "desktop appearance watcher retrying");
                    } else {
                        debug!("desktop appearance signal stream ended; reconnecting");
                    }
                    thread::sleep(RETRY);
                }
            });
        if let Err(error) = spawned {
            warn!(
                %error,
                "could not start the desktop appearance watcher — the card keeps the window's appearance"
            );
        }
    }

    fn listen(appearance: &Appearance) -> zbus::Result<()> {
        let connection = Connection::session()?;
        let proxy = SettingsProxyBlocking::new(&connection)?;
        // Wait for the portal to answer: activating it can fail before the
        // user session is ready, and a backend without the appearance
        // namespace never answers at all.
        let mut value = None;
        for _ in 0..WAIT_ATTEMPTS {
            match proxy.read(NAMESPACE, COLOR_SCHEME) {
                Ok(read) => {
                    value = Some(read);
                    break;
                }
                Err(error) => {
                    debug!(%error, "the settings portal does not report a color scheme yet");
                    thread::sleep(RETRY);
                }
            }
        }
        let Some(value) = value else {
            return Err(zbus::Error::Failure(
                "the settings portal never reported a color scheme".into(),
            ));
        };
        appearance.set_dark(color_scheme(&value));
        info!(
            dark = appearance.is_dark().unwrap_or(false),
            "following the desktop appearance"
        );
        for signal in proxy.receive_setting_changed()? {
            let args = signal.args()?;
            if *args.namespace() == NAMESPACE && *args.key() == COLOR_SCHEME {
                appearance.set_dark(color_scheme(args.value()));
            }
        }
        Ok(())
    }

    /// `color-scheme` is `u32`: 0 no preference, 1 prefer dark, 2 prefer light.
    /// Some portal versions wrap the value in an extra variant, so unwrap
    /// nested variants before reading the number.
    fn color_scheme(value: &OwnedValue) -> bool {
        fn number(value: &Value<'_>) -> Option<u32> {
            match value {
                Value::Value(inner) => number(inner),
                Value::U32(scheme) => Some(*scheme),
                _ => None,
            }
        }
        number(value) == Some(1)
    }

    #[cfg(test)]
    mod tests {
        use zbus::zvariant::{OwnedValue, Value};

        use super::color_scheme;

        #[test]
        fn the_portal_scheme_maps_no_preference_and_light_to_light() {
            let scheme = |value: Value<'static>| {
                let owned = OwnedValue::try_from(value).expect("owned value");
                color_scheme(&owned)
            };
            assert!(!scheme(Value::U32(0)));
            assert!(!scheme(Value::U32(2)));
            assert!(scheme(Value::U32(1)));
        }

        #[test]
        fn a_nested_variant_still_reads_the_scheme() {
            let nested = Value::Value(Box::new(Value::U32(1)));
            let owned = OwnedValue::try_from(nested).expect("owned value");
            assert!(color_scheme(&owned));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::Appearance;

    #[test]
    fn the_mode_is_unknown_until_the_desktop_reports_one() {
        // Off Linux nothing ever reports, so the card uses the window's own
        // appearance; on Linux this is the state before the portal answers.
        let appearance = Appearance::new();
        assert_eq!(appearance.is_dark(), None);
        assert_eq!(appearance.revision(), 0);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn the_revision_moves_only_when_the_mode_flips() {
        let appearance = Appearance::new();
        let start = appearance.revision();
        appearance.set_dark(true);
        assert_eq!(appearance.is_dark(), Some(true));
        assert!(appearance.revision() > start);
        let flipped = appearance.revision();
        appearance.set_dark(true);
        assert_eq!(appearance.revision(), flipped);
        appearance.set_dark(false);
        assert!(appearance.revision() > flipped);
    }
}
