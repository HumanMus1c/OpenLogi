//! The agent's Linux StatusNotifierItem tray.
//!
//! Mirrors the Windows notification-area and macOS menu-bar items: the
//! always-on agent hosts the icon, the GUI stays on demand. The icon is an
//! ARGB32 battery badge (the same renderer Windows uses) delivered over the
//! `org.kde.StatusNotifierItem` D-Bus interface, with the device rows in a
//! `com.canonical.dbusmenu` menu.
//!
//! Whether the icon appears at all is the desktop's decision: KDE hosts SNI
//! natively, GNOME needs the AppIndicator extension, and XFCE needs an
//! indicator plugin. With no host the registration is a soft failure — the
//! agent keeps working headless, exactly as it does without a tray today.

use std::path::Path;
use std::process::Command;
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use ksni::menu::{MenuItem, StandardItem};
use ksni::{OfflineReason, ToolTip, Tray, TrayMethods as _};
use openlogi_agent_core::observable::ObservableState;
use openlogi_core::config::{Config, TrayBadgeStyle};
use sysinfo::{Pid, ProcessesToUpdate, Signal, System};
use tokio::sync::mpsc::{self, UnboundedReceiver, UnboundedSender};
use tracing::{debug, info, warn};

use crate::shutdown::{self, ShutdownRequestSender};
use crate::tray_readout::{BRAND, Badge, TrayReadout, WideBadge, render_badge, render_wide};

/// The GUI binary installed beside the agent (package and cargo target dirs
/// use the same layout).
const GUI_BINARY: &str = "openlogi-desktop";
/// The panel's icon slots. SNI hosts pick the closest bitmap; the renderer
/// clamps to its own 64 px ceiling.
const ICON_SIZES: [u16; 5] = [16, 22, 24, 32, 48];
/// The wide face's heights: the usual panel slot and a 2x variant for
/// HiDPI panels. The width follows the content.
const WIDE_HEIGHTS: [u32; 2] = [22, 44];
/// StatusNotifierItem carries no panel-theme signal, so the badge ring and the
/// brand glyph assume the common dark panel. A light panel would need a
/// theme-aware icon, which is a separate follow-up.
const LIGHT_PANEL: bool = false;

/// A live settings or inventory change for the tray thread.
#[derive(Clone, Debug)]
enum TrayCommand {
    Battery(bool),
    BadgeStyle(TrayBadgeStyle),
    /// A config reload changed the persisted device names the rows show.
    Config(Arc<Config>),
}

/// The channel a config reload and the inventory watcher share.
static SENDER: OnceLock<UnboundedSender<TrayCommand>> = OnceLock::new();

/// Start the tray on its own D-Bus thread. No-op when the user hid the tray
/// preference; that takes effect on the agent's next launch, as on the other
/// platforms.
pub fn spawn(
    show_in_tray: bool,
    tray_battery: bool,
    badge_style: TrayBadgeStyle,
    config: Arc<Config>,
    observable: Arc<ObservableState>,
    shutdown_tx: ShutdownRequestSender,
) {
    if !show_in_tray {
        info!("tray icon disabled by preference — agent stays invisible");
        return;
    }
    let (sender, commands) = mpsc::unbounded_channel();
    if SENDER.set(sender).is_err() {
        warn!("the Linux tray is already running — keeping the first instance");
        return;
    }
    if let Err(error) = openlogi_core::worker::spawn("openlogi-tray", move |runtime| {
        run(
            &runtime,
            observable,
            shutdown_tx,
            tray_battery,
            badge_style,
            config,
            commands,
        );
    }) {
        warn!(%error, "could not spawn the Linux tray thread");
    }
}

/// Apply a config reload's `tray_battery` choice. A no-op when the tray was
/// hidden at launch (the setting is read at startup only) or had no D-Bus
/// session to attach to.
pub fn set_battery_readout(enabled: bool) {
    send(TrayCommand::Battery(enabled));
}

/// Apply a config reload's `tray_badge_style` choice.
pub fn set_badge_style(style: TrayBadgeStyle) {
    send(TrayCommand::BadgeStyle(style));
}

/// Hand a reloaded config to the tray, so rows pick up changed device names.
pub fn set_config(config: Arc<Config>) {
    send(TrayCommand::Config(config));
}

fn send(command: TrayCommand) {
    if let Some(sender) = SENDER.get() {
        let _ = sender.send(command);
    }
}

/// One rendered state: what the tooltip, icon, and menu read.
#[derive(Clone)]
struct TrayView {
    tooltip: String,
    icons: Vec<ksni::Icon>,
    rows: Vec<String>,
}

impl TrayView {
    fn build(summary: &TrayReadout, battery: bool, style: TrayBadgeStyle) -> Self {
        let style = desktop_style(style);
        if !battery {
            return Self {
                tooltip: String::new(),
                icons: brand_icons(),
                rows: Vec::new(),
            };
        }
        let icons = if style == TrayBadgeStyle::Wide {
            let badge = summary.wide_badge();
            WIDE_HEIGHTS
                .iter()
                .map(|&height| wide_icon(&badge, height))
                .collect()
        } else {
            let badge = summary.badge();
            ICON_SIZES
                .iter()
                .map(|&size| badge_icon(badge, size, style))
                .collect()
        };
        Self {
            tooltip: summary.battery_readout().unwrap_or_default(),
            icons,
            rows: summary.menu_lines().collect(),
        }
    }
}

struct OpenLogiTray {
    observable: Arc<ObservableState>,
    shutdown_tx: ShutdownRequestSender,
    battery: bool,
    badge_style: TrayBadgeStyle,
    config: Arc<Config>,
    view: TrayView,
}

impl OpenLogiTray {
    fn new(
        observable: Arc<ObservableState>,
        shutdown_tx: ShutdownRequestSender,
        battery: bool,
        badge_style: TrayBadgeStyle,
        config: Arc<Config>,
    ) -> Self {
        let mut tray = Self {
            observable,
            shutdown_tx,
            battery,
            badge_style,
            config,
            view: TrayView {
                tooltip: String::new(),
                icons: Vec::new(),
                rows: Vec::new(),
            },
        };
        tray.refresh();
        tray
    }

    fn refresh(&mut self) {
        let summary = self.observable.read(|snapshot| {
            TrayReadout::from_parts(
                &snapshot.inventory,
                &snapshot.standalone,
                &snapshot.lights,
                Some(&self.config),
            )
        });
        self.view = TrayView::build(&summary, self.battery, self.badge_style);
    }

    fn apply(&mut self, command: TrayCommand) {
        match command {
            TrayCommand::Battery(enabled) => {
                self.battery = enabled;
                self.refresh();
            }
            TrayCommand::BadgeStyle(style) => {
                self.badge_style = style;
                self.refresh();
            }
            TrayCommand::Config(config) => {
                self.config = config;
                self.refresh();
            }
        }
    }

    fn quit(&mut self) {
        terminate_gui();
        crate::overlay::evict_on_quit();
        info!("tray Quit — requesting graceful agent shutdown");
        shutdown::request_tray_quit(Some(&self.shutdown_tx), 0);
    }
}

impl Tray for OpenLogiTray {
    fn id(&self) -> String {
        "openlogi".to_owned()
    }

    fn category(&self) -> ksni::Category {
        ksni::Category::Hardware
    }

    fn title(&self) -> String {
        BRAND.to_owned()
    }

    fn status(&self) -> ksni::Status {
        ksni::Status::Active
    }

    fn icon_pixmap(&self) -> Vec<ksni::Icon> {
        self.view.icons.clone()
    }

    fn tool_tip(&self) -> ToolTip {
        ToolTip {
            title: BRAND.to_owned(),
            description: self.view.tooltip.clone(),
            ..ToolTip::default()
        }
    }

    fn activate(&mut self, _x: i32, _y: i32) {
        open_gui();
    }

    fn menu(&self) -> Vec<MenuItem<Self>> {
        let mut items = Vec::new();
        for row in &self.view.rows {
            items.push(
                StandardItem {
                    label: row.clone(),
                    enabled: false,
                    ..StandardItem::default()
                }
                .into(),
            );
        }
        if !self.view.rows.is_empty() {
            items.push(MenuItem::Separator);
        }
        items.push(
            StandardItem {
                label: rust_i18n::t!("app.show_main_window").to_string(),
                activate: Box::new(|_| open_gui()),
                ..StandardItem::default()
            }
            .into(),
        );
        items.push(MenuItem::Separator);
        items.push(
            StandardItem {
                label: rust_i18n::t!("app.quit_openlogi").to_string(),
                activate: Box::new(|tray: &mut Self| tray.quit()),
                ..StandardItem::default()
            }
            .into(),
        );
        items
    }

    fn watcher_offline(&self, reason: OfflineReason) -> bool {
        debug!(?reason, "tray — no StatusNotifier host; waiting for one");
        true
    }
}

/// Run the tray service for the process lifetime: register with the watcher,
/// then fold inventory changes and config reloads into it.
fn run(
    runtime: &tokio::runtime::Runtime,
    observable: Arc<ObservableState>,
    shutdown_tx: ShutdownRequestSender,
    battery: bool,
    badge_style: TrayBadgeStyle,
    config: Arc<Config>,
    mut commands: UnboundedReceiver<TrayCommand>,
) {
    runtime.block_on(async move {
        let mut watch = observable.subscribe();
        let tray = OpenLogiTray::new(observable, shutdown_tx, battery, badge_style, config);
        // A desktop without an SNI host is a soft failure: keep the service
        // alive so it appears if the host starts later (a panel restart, or a
        // GNOME session that loads its AppIndicator extension late).
        let handle = match tray.assume_sni_available(true).spawn().await {
            Ok(handle) => handle,
            Err(error) => {
                warn!(%error, "could not start the Linux tray");
                return;
            }
        };
        info!("StatusNotifierItem tray installed");

        loop {
            tokio::select! {
                command = commands.recv() => {
                    let Some(command) = command else {
                        break;
                    };
                    if handle.update(move |tray| tray.apply(command)).await.is_none() {
                        warn!("the Linux tray service stopped");
                        break;
                    }
                }
                changed = watch.changed() => {
                    if changed.is_err() {
                        break;
                    }
                    if handle.update(OpenLogiTray::refresh).await.is_none() {
                        warn!("the Linux tray service stopped");
                        break;
                    }
                }
            }
        }
    });
}

/// The style the running desktop can actually render.
///
/// XFCE's systray scales every tray item into the panel's icon slot, so the
/// wide face is unreadable there and falls back to the plain badge. Other
/// desktops keep the configured style — the wide face stays an opt-in
/// experiment, confined to the environments that can show it.
fn supported_style(style: TrayBadgeStyle, desktop: &str) -> TrayBadgeStyle {
    let xfce = desktop.to_ascii_lowercase().contains("xfce");
    if style == TrayBadgeStyle::Wide && xfce {
        TrayBadgeStyle::Circle
    } else {
        style
    }
}

/// [`supported_style`] against the session's desktop, read once.
fn desktop_style(style: TrayBadgeStyle) -> TrayBadgeStyle {
    static DESKTOP: OnceLock<String> = OnceLock::new();
    static FALLBACK_LOGGED: OnceLock<()> = OnceLock::new();
    let desktop = DESKTOP.get_or_init(desktop_name);
    let supported = supported_style(style, desktop);
    if supported != style && FALLBACK_LOGGED.set(()).is_ok() {
        info!(
            desktop = %desktop,
            "this desktop scales tray items into the panel's icon slot — using the plain tray badge instead of the wide face"
        );
    }
    supported
}

/// The session's desktop name. The environment is the primary signal; an
/// agent started outside the desktop's own environment (a terminal, a user
/// unit) still finds XFCE through its panel process, which is the SNI host
/// whose icon slot the style has to fit.
fn desktop_name() -> String {
    let from_env = [
        "XDG_CURRENT_DESKTOP",
        "XDG_SESSION_DESKTOP",
        "DESKTOP_SESSION",
    ]
    .iter()
    .find_map(|name| std::env::var(name).ok())
    .filter(|value| !value.trim().is_empty());
    if let Some(desktop) = from_env {
        return desktop;
    }
    let mut system = System::new();
    system.refresh_processes(ProcessesToUpdate::All, true);
    if system.processes().values().any(|process| {
        process
            .name()
            .to_string_lossy()
            .to_ascii_lowercase()
            .contains("xfce4-panel")
    }) {
        return "XFCE".to_owned();
    }
    String::new()
}

/// Render `badge` at `size` as the ARGB32 pixmap SNI expects.
fn badge_icon(badge: Badge, size: u16, style: TrayBadgeStyle) -> ksni::Icon {
    let rgba = render_badge(badge, u32::from(size), LIGHT_PANEL, style);
    let extent = i32::from(size);
    argb_icon(rgba, extent, extent)
}

/// Render the wide face at `height` as the ARGB32 pixmap SNI expects; the
/// width follows the content.
fn wide_icon(badge: &WideBadge, height: u32) -> ksni::Icon {
    let (width, rgba) = render_wide(badge, height, LIGHT_PANEL);
    argb_icon(
        rgba,
        i32::try_from(width).unwrap_or(0),
        i32::try_from(height).unwrap_or(0),
    )
}

/// Turn straight-alpha RGBA into SNI's network-byte-order ARGB32.
fn argb_icon(mut rgba: Vec<u8>, width: i32, height: i32) -> ksni::Icon {
    for pixel in rgba.as_chunks_mut::<4>().0 {
        pixel.rotate_right(1);
    }
    ksni::Icon {
        width,
        height,
        data: rgba,
    }
}

/// The static brand glyph used while the battery readout is switched off.
fn brand_icons() -> Vec<ksni::Icon> {
    static DARK: OnceLock<Vec<ksni::Icon>> = OnceLock::new();
    static LIGHT: OnceLock<Vec<ksni::Icon>> = OnceLock::new();
    let cache = if LIGHT_PANEL { &LIGHT } else { &DARK };
    cache
        .get_or_init(|| {
            let png: &[u8] = if LIGHT_PANEL {
                include_bytes!("../assets/tray-icon@2x.png")
            } else {
                include_bytes!("../assets/tray-icon-white@2x.png")
            };
            decode_png_icon(png).into_iter().collect()
        })
        .clone()
}

fn decode_png_icon(bytes: &[u8]) -> Option<ksni::Icon> {
    let mut decoder = png::Decoder::new(std::io::Cursor::new(bytes));
    decoder.set_transformations(png::Transformations::normalize_to_color8());
    let mut reader = decoder.read_info().ok()?;
    let mut buffer = vec![0; reader.output_buffer_size()?];
    let info = reader.next_frame(&mut buffer).ok()?;
    let source = &buffer[..info.buffer_size()];
    let rgba = match info.color_type {
        png::ColorType::Rgba => source.to_vec(),
        png::ColorType::GrayscaleAlpha => source
            .as_chunks::<2>()
            .0
            .iter()
            .flat_map(|pixel| [pixel[0], pixel[0], pixel[0], pixel[1]])
            .collect(),
        _ => return None,
    };
    let width = i32::try_from(info.width).ok()?;
    let height = i32::try_from(info.height).ok()?;
    Some(argb_icon(rgba, width, height))
}

/// Bring the GUI up. The GUI is a single-instance process, so an already
/// running one just stays where it is; its taskbar button is the way back.
fn open_gui() {
    if !gui_pids().is_empty() {
        info!("tray — the GUI is already running; use its taskbar button");
        return;
    }
    let Ok(executable) = std::env::current_exe() else {
        warn!("could not resolve the agent's own path — cannot launch the GUI");
        return;
    };
    let Some(directory) = executable.parent() else {
        return;
    };
    let gui = directory.join(GUI_BINARY);
    if !gui.is_file() {
        warn!(path = %gui.display(), "no GUI binary next to the agent");
        return;
    }
    match Command::new(&gui).spawn() {
        Ok(_) => info!(path = %gui.display(), "tray — launched the GUI"),
        Err(error) => warn!(%error, path = %gui.display(), "tray — could not launch the GUI"),
    }
}

/// Quit the whole app: the GUI first (its IPC retry loop would otherwise
/// respawn the agent we are about to exit), then the agent core.
fn terminate_gui() {
    let pids = gui_pids();
    if pids.is_empty() {
        return;
    }
    let mut system = System::new();
    system.refresh_processes(ProcessesToUpdate::All, true);
    for pid in &pids {
        let Some(process) = system.process(*pid) else {
            continue;
        };
        if process.kill_with(Signal::Term) == Some(true) {
            info!(pid = pid.as_u32(), "tray Quit — asked the GUI to terminate");
        } else {
            warn!(
                pid = pid.as_u32(),
                "tray Quit — could not terminate the GUI"
            );
        }
    }
    let deadline = Instant::now() + Duration::from_millis(1500);
    while Instant::now() < deadline {
        system.refresh_processes(ProcessesToUpdate::All, true);
        if pids.iter().all(|pid| system.process(*pid).is_none()) {
            return;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    warn!("the GUI did not exit after SIGTERM — the agent still quits");
}

/// This user's running GUI processes.
fn gui_pids() -> Vec<Pid> {
    let mut system = System::new();
    system.refresh_processes(ProcessesToUpdate::All, true);
    let own_user = system
        .process(Pid::from_u32(std::process::id()))
        .and_then(sysinfo::Process::user_id);
    system
        .processes()
        .iter()
        .filter(|(_, process)| {
            is_gui_process(process) && (own_user.is_none() || process.user_id() == own_user)
        })
        .map(|(pid, _)| *pid)
        .collect()
}

fn is_gui_process(process: &sysinfo::Process) -> bool {
    process
        .exe()
        .and_then(Path::file_name)
        .is_some_and(|name| name == std::ffi::OsStr::new(GUI_BINARY))
        || process.name().to_string_lossy() == GUI_BINARY
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use ksni::Tray as _;
    use openlogi_agent_core::observable::ObservableState;
    use openlogi_core::device::{
        BatteryInfo, BatteryLevel, BatteryStatus, DeviceInventory, DeviceKind, PairedDevice,
        ReceiverInfo,
    };
    use openlogi_ipc::InventoryHealth;

    use super::{
        ICON_SIZES, OpenLogiTray, TrayCommand, WIDE_HEIGHTS, argb_icon, badge_icon,
        decode_png_icon, supported_style,
    };
    use crate::tray_readout::Badge;
    use openlogi_core::config::TrayBadgeStyle;

    fn inventory() -> DeviceInventory {
        DeviceInventory {
            receiver: ReceiverInfo {
                name: "Logi Bolt Receiver".to_owned(),
                vendor_id: 0x046d,
                product_id: 0xc548,
                unique_id: None,
            },
            paired: vec![PairedDevice {
                slot: 1,
                codename: Some("MX Master 3S".to_owned()),
                wpid: None,
                kind: DeviceKind::Mouse,
                online: true,
                battery: Some(BatteryInfo {
                    percentage: Some(64),
                    level: BatteryLevel::Good,
                    status: BatteryStatus::Discharging,
                }),
                model_info: None,
                capabilities: None,
                settings_revision: 0,
            }],
        }
    }

    fn tray(battery: bool) -> OpenLogiTray {
        let observable = Arc::new(ObservableState::new("test".to_owned()));
        observable.set_inventory(InventoryHealth::Ready, &[inventory()], &[], false);
        let (shutdown_tx, _requests) = crate::shutdown::request_channel();
        OpenLogiTray::new(
            observable,
            shutdown_tx,
            battery,
            TrayBadgeStyle::Circle,
            std::sync::Arc::new(openlogi_core::config::Config::default()),
        )
    }

    #[test]
    fn rgba_becomes_network_order_argb() {
        let icon = argb_icon(vec![1, 2, 3, 4], 1, 1);
        assert_eq!(icon.data, [4, 1, 2, 3]);
        assert_eq!((icon.width, icon.height), (1, 1));
    }

    #[test]
    fn the_badge_renders_at_the_requested_size() {
        let icon = badge_icon(Badge::none(), 22, TrayBadgeStyle::Circle);
        assert_eq!((icon.width, icon.height), (22, 22));
        assert_eq!(icon.data.len(), 22 * 22 * 4);
    }

    #[test]
    fn the_tray_reads_the_agent_inventory() {
        let tray = tray(true);
        assert_eq!(tray.tool_tip().description, "MX Master 3S 64%");
        assert_eq!(tray.icon_pixmap().len(), ICON_SIZES.len());
        let first = tray.menu();
        let ksni::MenuItem::Standard(row) = &first[0] else {
            panic!("device row missing");
        };
        assert!(row.label.contains("64%"));
        assert!(!row.enabled);
    }

    #[test]
    fn switching_the_readout_off_restores_the_brand_icon() {
        let mut tray = tray(true);
        let badge = tray.icon_pixmap();
        tray.apply(TrayCommand::Battery(false));
        assert!(tray.tool_tip().description.is_empty());
        assert_ne!(tray.icon_pixmap()[0].data, badge[0].data);
        assert!(!tray.icon_pixmap().is_empty());

        tray.apply(TrayCommand::Battery(true));
        let circle = tray.icon_pixmap()[0].data.clone();
        tray.apply(TrayCommand::BadgeStyle(TrayBadgeStyle::RoundedSquare));
        assert_ne!(tray.icon_pixmap()[0].data, circle);
    }

    #[test]
    fn xfce_keeps_the_plain_badge_instead_of_the_wide_face() {
        use openlogi_core::config::TrayBadgeStyle;

        for desktop in ["XFCE", "XFCE:GNOME", "xfce"] {
            assert_eq!(
                supported_style(TrayBadgeStyle::Wide, desktop),
                TrayBadgeStyle::Circle,
                "{desktop}"
            );
        }
        // The square badge is a plain icon and stays as configured.
        assert_eq!(
            supported_style(TrayBadgeStyle::RoundedSquare, "XFCE"),
            TrayBadgeStyle::RoundedSquare
        );
        // Desktops that can keep the aspect ratio keep the wide face.
        for desktop in ["ubuntu:GNOME", "KDE", "sway"] {
            assert_eq!(
                supported_style(TrayBadgeStyle::Wide, desktop),
                TrayBadgeStyle::Wide,
                "{desktop}"
            );
        }
    }

    #[test]
    fn the_wide_style_renders_the_name_the_bar_and_the_value_in_one_row() {
        let mut tray = tray(true);
        tray.apply(TrayCommand::BadgeStyle(TrayBadgeStyle::Wide));
        let icons = tray.icon_pixmap();
        assert_eq!(icons.len(), WIDE_HEIGHTS.len());
        for icon in &icons {
            assert!(
                icon.width > icon.height,
                "the wide face earns its name: {}x{}",
                icon.width,
                icon.height
            );
            assert_eq!(
                icon.data.len(),
                usize::try_from(icon.width * icon.height * 4).expect("bounded icon")
            );
        }
    }

    #[test]
    fn the_brand_assets_decode_to_argb() {
        for png in [
            include_bytes!("../assets/tray-icon@2x.png").as_slice(),
            include_bytes!("../assets/tray-icon-white@2x.png").as_slice(),
            include_bytes!("../assets/tray-icon-prism@2x.png").as_slice(),
        ] {
            let icon = decode_png_icon(png).expect("the packaged tray PNG decodes");
            assert!(icon.width > 0 && icon.height > 0);
            assert_eq!(
                icon.data.len(),
                usize::try_from(icon.width * icon.height * 4).expect("bounded icon")
            );
        }
    }
}
