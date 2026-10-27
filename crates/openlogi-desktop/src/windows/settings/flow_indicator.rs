//! Flow Indicator settings page: the tray readout and the floating card.

use super::{
    AppState, IconName, InteractiveElement as _, SettingField, SettingGroup, SettingItem,
    SettingPage,
};
use gpui_component::radio::{Radio, RadioGroup};
use openlogi_core::config::{
    FLOW_INDICATOR_MAX_DEVICES_DEFAULT, FLOW_INDICATOR_MAX_DEVICES_MAX,
    FLOW_INDICATOR_MAX_DEVICES_MIN, FlowIndicatorDisplayMode, TrayBadgeStyle,
};

pub(super) fn flow_indicator_page() -> SettingPage {
    let group = SettingGroup::new();
    // Linux has no tray, and the macOS menu-bar readout lands with its parity
    // work, so the switch appears only where it currently drives something.
    #[cfg(target_os = "windows")]
    let group = group.item(tray_battery_item());
    // The badge is Windows-only: macOS reads the level out as text and Linux
    // has no tray surface.
    #[cfg(target_os = "windows")]
    let group = group.item(badge_style_item());
    let group = group
        .item(flow_indicator_item())
        .item(show_offline_item())
        .item(max_devices_item())
        .item(display_mode_item());
    SettingPage::new(tr!("flow_indicator.flow_indicator"))
        .icon(IconName::BatteryFull)
        .resettable(false)
        .group(group)
}

#[cfg(target_os = "windows")]
fn tray_battery_item() -> SettingItem {
    SettingItem::new(
        tr!("flow_indicator.tray_battery"),
        SettingField::switch(
            |cx| AppState::try_read(cx).is_some_and(|state| state.app_settings().tray_battery),
            |enabled, cx| {
                AppState::apply(cx, |state| state.commit_tray_battery(enabled));
            },
        ),
    )
    .description(tr!("flow_indicator.tray_battery_description"))
}

#[cfg(target_os = "windows")]
fn badge_style_item() -> SettingItem {
    SettingItem::new(
        tr!("flow_indicator.tray_badge_style"),
        SettingField::render(|_, _, cx| {
            let style = AppState::try_read(cx).map_or(TrayBadgeStyle::Circle, |state| {
                state.app_settings().tray_badge_style
            });
            RadioGroup::vertical("tray-badge-style")
                .child(
                    Radio::new("tray-badge-style-circle")
                        .label(tr!("flow_indicator.tray_badge_style_circle"))
                        .debug_selector(|| "tray-badge-style-circle".into()),
                )
                .child(
                    Radio::new("tray-badge-style-square")
                        .label(tr!("flow_indicator.tray_badge_style_square"))
                        .debug_selector(|| "tray-badge-style-square".into()),
                )
                .selected_index(Some(match style {
                    TrayBadgeStyle::Circle => 0,
                    TrayBadgeStyle::RoundedSquare => 1,
                }))
                .on_change(|index, _, cx| {
                    let style = match index {
                        0 => TrayBadgeStyle::Circle,
                        1 => TrayBadgeStyle::RoundedSquare,
                        _ => unreachable!("the badge style has exactly two choices"),
                    };
                    AppState::apply(cx, |state| state.commit_tray_badge_style(style));
                })
        }),
    )
    .layout(gpui::Axis::Vertical)
    .description(tr!("flow_indicator.tray_badge_style_description"))
}

fn flow_indicator_item() -> SettingItem {
    SettingItem::new(
        tr!("flow_indicator.floating_indicator"),
        SettingField::switch(
            |cx| AppState::try_read(cx).is_some_and(|state| state.app_settings().flow_indicator),
            |enabled, cx| {
                AppState::apply(cx, |state| state.commit_flow_indicator(enabled));
            },
        ),
    )
    .description(tr!("flow_indicator.floating_indicator_description"))
}

fn show_offline_item() -> SettingItem {
    SettingItem::new(
        tr!("flow_indicator.show_offline"),
        SettingField::switch(
            |cx| {
                AppState::try_read(cx)
                    .is_some_and(|state| state.app_settings().flow_indicator_show_offline)
            },
            |enabled, cx| {
                AppState::apply(cx, |state| {
                    state.commit_flow_indicator_show_offline(enabled)
                });
            },
        ),
    )
    .description(tr!("flow_indicator.show_offline_description"))
}

fn max_devices_item() -> SettingItem {
    SettingItem::new(
        tr!("flow_indicator.max_devices"),
        SettingField::render(|_, _, cx| {
            let max_devices = AppState::try_read(cx)
                .map_or(FLOW_INDICATOR_MAX_DEVICES_DEFAULT, |state| {
                    state.app_settings().flow_indicator_row_limit()
                });
            let mut group = RadioGroup::horizontal("flow-indicator-max-devices");
            for value in FLOW_INDICATOR_MAX_DEVICES_MIN..=FLOW_INDICATOR_MAX_DEVICES_MAX {
                group = group.child(
                    Radio::new(format!("flow-indicator-max-{value}"))
                        .label(value.to_string())
                        .debug_selector(move || format!("flow-indicator-max-{value}")),
                );
            }
            group
                .selected_index(Some(usize::from(
                    max_devices - FLOW_INDICATOR_MAX_DEVICES_MIN,
                )))
                .on_change(|index, _, cx| {
                    let value = FLOW_INDICATOR_MAX_DEVICES_MIN + u8::try_from(*index).unwrap_or(0);
                    AppState::apply(cx, |state| state.commit_flow_indicator_max_devices(value));
                })
        }),
    )
    .layout(gpui::Axis::Vertical)
    .description(tr!("flow_indicator.max_devices_description"))
}

fn display_mode_item() -> SettingItem {
    SettingItem::new(
        tr!("flow_indicator.display_mode"),
        SettingField::render(|_, _, cx| {
            let mode = AppState::try_read(cx).map_or(FlowIndicatorDisplayMode::Normal, |state| {
                state.app_settings().flow_indicator_display_mode
            });
            RadioGroup::vertical("flow-indicator-display-mode")
                .child(
                    Radio::new("flow-indicator-mode-normal")
                        .label(tr!("flow_indicator.mode_normal"))
                        .debug_selector(|| "flow-indicator-mode-normal".into()),
                )
                .child(
                    Radio::new("flow-indicator-mode-topmost")
                        .label(tr!("flow_indicator.mode_topmost"))
                        .debug_selector(|| "flow-indicator-mode-topmost".into()),
                )
                .selected_index(Some(match mode {
                    FlowIndicatorDisplayMode::Normal => 0,
                    FlowIndicatorDisplayMode::Topmost => 1,
                }))
                .on_change(|index, _, cx| {
                    let mode = match index {
                        0 => FlowIndicatorDisplayMode::Normal,
                        1 => FlowIndicatorDisplayMode::Topmost,
                        _ => unreachable!("the display mode has exactly two choices"),
                    };
                    AppState::apply(cx, |state| state.commit_flow_indicator_display_mode(mode));
                })
        }),
    )
    .layout(gpui::Axis::Vertical)
    .description(tr!("flow_indicator.display_mode_description"))
}
