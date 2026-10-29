//! The tray-facing app settings a config reload carries.
//!
//! Both the real agent ([`crate::server`]) and the demo tree apply a GUI save
//! to the tray the same way; this is that single place. Everything else a
//! reload carries — the inventory, autostart, dispatcher state — belongs to
//! the caller.

use openlogi_core::config::Config;

/// Push `config`'s tray settings to the tray this process hosts. The language
/// goes first: the tray builds its words in the new locale.
pub(crate) fn apply_tray_settings(config: &Config) {
    let settings = &config.app_settings;
    openlogi_core::locale::activate(settings.language.as_deref());
    #[cfg(target_os = "macos")]
    {
        use std::sync::Arc;

        crate::tray::set_icon(settings.app_icon);
        crate::tray::relocalize();
        crate::tray::set_battery_readout(settings.tray_battery);
        crate::tray::set_config(Arc::new(config.clone()));
    }
    #[cfg(target_os = "windows")]
    {
        use std::sync::Arc;

        crate::tray_windows::set_battery_readout(settings.tray_battery);
        crate::tray_windows::set_badge_style(settings.tray_badge_style);
        crate::tray_windows::set_config(Arc::new(config.clone()));
    }
    #[cfg(target_os = "linux")]
    {
        use std::sync::Arc;

        crate::tray_linux::set_battery_readout(settings.tray_battery);
        crate::tray_linux::set_badge_style(settings.tray_badge_style);
        crate::tray_linux::set_config(Arc::new(config.clone()));
    }
}
