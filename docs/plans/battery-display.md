# Battery display: tray readout and floating widget

Status: M0, M1a, M1b (Windows tray badge), and the Flow Indicator (floating
card + Settings tab, drag and position memory, Normal/Topmost) implemented.
The macOS menu-bar readout (title beside the glyph, capped device rows, live
`tray_battery` toggle) is implemented but not yet compiled or run on macOS
hardware. Windows live checks done on a 125%-DPI dark/light taskbar: live
tooltip, badge render, theme-change refresh, and Explorer-restart re-add.
Linux (2026-10-28) was verified in real compositors — an XFCE/X11 session
over xrdp and a nested labwc (wlroots) inside WSLg, the latter because the
VM has no `/dev/dri` and WSLg's copy mode only presents software (pixman)
surfaces. The card renders, updates live, closes on the settings switch,
shows offline rows with their last reading and hides when empty; Normal is
coverable and Topmost stays above a terminal. Wayland now uses a layer
surface: borderless, anchored bottom-right (24 px), Normal on the bottom
layer and Topmost on the top layer. X11 and the plain-window fallback are
borderless through client-side decorations. Still missing on Linux: card
drag (GPUI has no window-control-area drag on Wayland or X11) and position
restore (the WM/compositor places ordinary windows; layer surfaces are
anchored). Remaining: macOS hardware verification, GNOME/KDE desktops
(GNOME has no wlr-layer-shell; the plain fallback applies), the 100/150/200%
DPI matrix beyond the observed 125%, and drag. GPUI exposes no bottom-most
window layer, so there is no Desktop display mode; the card's position lives
in `flow-indicator.toml` beside `config.toml`.

The visual reference is **vigil-watt** (MIT): its tray icon renders the lowest
battery percentage as a coloured badge, its tray menu lists device readings, and
its desktop widget is a small draggable card with one row per device. FerrisPulse
is referenced only as a behaviour precedent; it is GPLv3 and none of its source,
bitmaps, or hand-drawn digit arrays may enter OpenLogi.

Out of scope:

- the Windows taskbar-embedded pill (`TaskbarPillWindow` in vigil-watt);
- new device support, HID/HID++ work, or multi-brand scanning;
- battery history, charts, or remaining-time estimates;
- replacing the existing low-battery notice (the overlay already has one).

## 1. Goal

Give the user a glanceable, always-available battery readout without opening the
settings app:

1. **Tray:** at-a-glance active/first-device level, plus a menu with every
   device's reading.
2. **Floating widget:** a compact card in the overlay process showing a small
   list of devices, their percentages, and charging state.

Product decisions confirmed on 2026-10-27: both surfaces default **on**; the
tray shows the device currently in use, falling back to the first device with a
reading; the widget starts bottom-right with at most 3 rows in `Normal` mode;
offline devices remain visible with their last-known reading; the overlay reads
`config.toml` directly instead of adding an IPC settings channel.

Success criteria:

- every battery source in the widget/tray agrees with the GUI's reading and
  thresholds;
- neither surface causes a new HID read, a new permission prompt, or an
  elevation request;
- each surface has a switch, and turning both off restores today's UI exactly;
- the feature can ship without changing the IPC protocol.

## 2. What already exists

- **Battery data is already on the IPC contract.** `PairedDevice.battery:
  Option<BatteryInfo>` carries percentage, firmware level, and charging status
  ([device.rs](../../crates/openlogi-core/src/device.rs#L559)); `BatteryInfo`
  already owns the low-battery decision in `needs_attention`
  ([device.rs](../../crates/openlogi-core/src/device.rs#L339)).
- **The overlay can already see the full state.** `AgentSnapshot` contains the
  complete inventory ([ipc.rs](../../crates/openlogi-ipc/src/ipc.rs#L259)) and
  `Observer::state` wraps the existing whole-state long poll
  ([client.rs](../../crates/openlogi-ipc/src/client.rs#L309)). `set_inventory`
  compares the whole inventory, so battery-only changes advance the generation
  ([observable.rs](../../crates/openlogi-agent-core/src/observable.rs#L158)).
- **The overlay is the right host.** It is a warm, always-on GPUI process
  supervised by the agent ([overlay.rs](../../crates/openlogi-agent/src/overlay.rs#L1)),
  and already draws transparent pop-up windows, notice cards, and indicators
  ([notice.rs](../../crates/openlogi-overlay/src/notice.rs#L147)).
- **The tray already belongs to the agent.** Windows uses
  `Shell_NotifyIcon` on its own thread
  ([tray_windows.rs](../../crates/openlogi-agent/src/tray_windows.rs#L71));
  macOS uses `NSStatusItem` ([tray.rs](../../crates/openlogi-agent/src/tray.rs#L85)).
  Both currently use a static brand glyph and only Show/Quit menu items
  ([tray_windows.rs](../../crates/openlogi-agent/src/tray_windows.rs#L259)).
- **The reading rules already exist in the GUI, but are private.** The desktop
  `battery.rs` owns the cold-start charging rule, level-only reading, and tone
  mapping ([battery.rs](../../crates/openlogi-desktop/src/ui/battery.rs#L16)).
  This is the logic the tray and widget must share, not re-derive.
- **The mock agent already drives battery states.** `battery 1 5`, `offline 1`,
  and the built-in demo inventory let both surfaces be tested without hardware
  ([DEVELOPMENT.md](../DEVELOPMENT.md#developing-the-gui-without-hardware)).

## 3. Product design

### 3.1 Shared reading rules

Use OpenLogi's semantics everywhere; do not copy vigil-watt's `≤20 / ≤50 / >50`
thresholds. The GUI's existing meanings are authoritative:

| Device state | Reading | Tone | Widget value | Tray badge |
|---|---|---|---|---|
| Charging at a cold-start 0% | no reading | Charging | `Charging` | bolt |
| Percentage available | `87%` | normal / low / critical by `needs_attention` | `87%` | `87` |
| Level only, no percentage | `Good` / `Low` / `Full` | level tone | level word | `–` |
| No reading | no reading | normal | `—` | `–` |
| Error status | no reading | Error | `Battery error` | red badge |
| Offline with last known | last reading, marked offline | muted | last value + `Offline` | tooltip/menu `Offline` |

The bar fill for a level-only reading is a coarse representation (`Critical 10`,
`Low 25`, `Good 70`, `Full 100`), not a charge estimate. A no-reading row keeps an
empty track and shows `—`; it must never become `0%`.

The low-battery rule is `BatteryInfo::needs_attention` only. It excludes charging
statuses and uses `≤20%` (or a low/critical firmware level when no percentage is
reported).

### 3.2 Tray

**Behaviour**

- Default: `tray_battery = true`. When switched off, the tray is exactly as it is
  today.
- When on, pick the device the GUI currently has selected (`selected_device`),
  when that selection can be matched to a paired device. Otherwise use the first
  online device with a battery reading in inventory order. If the preferred
  device has no reading, fall through to the next device with one; if none has a
  reading, show a neutral `–` badge and a "no devices" tooltip.
- Charging does not exclude a device; the tray shows a bolt for the selected
  device just as it would for any other. The goal is "the device I am using",
  not "the weakest battery".
- Tooltip: `OpenLogi · <device name> 87%` (or `… Charging` / `… Offline` /
  `OpenLogi`). Respect the `NOTIFYICONDATAW.szTip` limit; truncate on a character
  boundary.
- Right-click menu: keep Show/Quit; add a disabled list of device readings
  (name, percentage or `–`, charging bolt, offline word) above the separator.
  Cap the list at six rows so the menu stays usable; the widget shows the full
  list.
- Do **not** add a second low-battery balloon. The overlay notice already owns
  that cue.

The first implementation may fall back to "first online device with a reading"
if the persisted `selected_device` key cannot be matched without re-deriving
device config keys in the tray. Mapping the active device is a small follow-up,
not a reason to block the tray readout.

**Icon**

The first implementation uses a vigil-watt-style badge:

```text
   ┌─────────┐
   │  ( 87 ) │   tone-coloured disc, white digits
   └─────────┘
   charging: a bolt instead of digits
   no reading: a dash
```

- Render at the tray's small-icon size (`SM_CXSMICON`, normally 16/20/24 px),
  not by scaling a 32 px bitmap.
- Tone maps to a disc colour; carry a one-pixel contrast ring so it reads on
  light and dark taskbars. The white foreground is intentional.
- Keep the OpenLogi brand available as an option: the badge is controlled by the
  tray battery switch (default on), and a later variant may use the brand mark
  with a tone ring instead of digits.
- Fallback: if badge creation fails, keep the current brand glyph and log once.
  The tray must never disappear because a readout failed.

**Platform differences**

- Windows: dynamic `HICON`; update with `NIM_MODIFY`; destroy the previous
  `HICON`; re-add after `TaskbarCreated`; re-render on taskbar theme or DPI
  changes.
- macOS: the status item can show a title (`87%` / `⚡`); no bitmap text
  renderer is needed. This is a follow-up PR so the Windows renderer does not
  block the menu-bar side.
- Linux: a StatusNotifierItem tray (KDE natively; GNOME via the AppIndicator
  extension; XFCE via an indicator plugin) plus the widget. Without an SNI
  host the tray is simply absent; the widget still applies.

### 3.3 Floating widget

**Content**

```text
┌──────────────────────────────┐
│ [mouse]    MX Master 3S  87% │
│ ████████████████████░░░░     │
│ [keyboard] MX Keys      15%  │
│ ████░░░░░░░░░░░░░░░░░░░     │
└──────────────────────────────┘
```

- One row per device with a battery reading; default maximum 3 visible rows,
  configurable 1–5; more rows scroll.
- Row lanes: 22 px kind glyph, flexible name with ellipsis, optional status tag,
  charging bolt, right-aligned percentage. The percentage uses tabular numerals
  where the font supports them.
- Bar: 6 px, rounded, track from the surface palette, fill in the tone colour.
- Sort: discharging low first, charging/full last, no-reading last; stable
  tie-break by name.
- Empty state: hide the window when no device has a battery reading. Do not leave
  a permanent empty card on screen.
- Offline rows: show a last-known reading with a muted `Offline` marker. Use the
  reading still present in the inventory when it is there; otherwise use a
  session-scoped last-reading cache in the widget model, so a device that goes
  offline while the overlay is running keeps its last value. A device the user
  has never seen with a reading is hidden rather than shown as `0%`. This is a
  deliberate difference from vigil-watt, which only shows online devices, and
  matches OpenLogi's GUI "last known battery" language.

**Window**

- Borderless, transparent pop-up window; rounded card with a soft shadow; no
  title bar, no taskbar button, no Alt-Tab entry.
- Default position: bottom-right of the primary work area with a 24 px margin;
  restore the last position on the same display when possible, clamped to the
  work area. First-run position is replaceable; drag is part of the
  productization milestone.
- Display mode: `Normal` by default; `Topmost` and `Desktop` are opt-in. A
  topmost widget can cover fullscreen games, so it must not be the default.
- Follow the OS light/dark appearance. Use a solid translucent card and shadow
  first; true acrylic/blur is a later platform-specific enhancement, not a
  requirement.
- No focus steal: the window is non-activating; clicking it must not make the
  agent/GUI the active application. Drag may still require pointer capture.
- Right-click: optional later (`Show OpenLogi`); the essential on/off and mode
  controls live in Settings, reachable by keyboard.

**Geometry** (at 100% scale)

- Card width: 300 px; horizontal padding 16; vertical padding 14; radius 12.
- Row line: 22 px icon + 8 px gaps + 16 px percentage; name 13 px; percentage
  16 px semibold.
- Bar: 6 px high; gap between the line and bar 9 px; gap between rows 16 px.
- Three visible rows: 3 × 37 + 2 × 16 = 143 px content + 28 padding = 171 px.
- Five visible rows: 249 + 28 = 277 px.
- These are starting values, not a pixel contract; they must be measured in a
  real window before visual approval.

### 3.4 Colour roles

The overlay has no `gpui-component` theme, so the widget owns a small semantic
palette. The tones are the same meanings the GUI already uses; a surface may
map them differently, but it must not invent a second threshold.

| Role | Dark | Light | Notes |
|---|---|---|---|
| Card | `hsla(0, 0, 0.10, 0.94)` | `#FFFFFF` at 96% | transient notices use the dark card; a persistent desktop card follows OS appearance |
| Text | `#F5F5F7` | `#1D1D1F` | primary device name and percentage |
| Muted | `#A1A1A6` | `#6E6E73` | offline label, empty state, secondary status |
| Track | white at 12% | black at 8% | battery bar background |
| Border | white at 10% | black at 12% | card edge |
| Charging / Full | `#22C55E` | same | `STATUS_CONNECTED` |
| Low | `#EAB308` | same | `STATUS_CONNECTING` |
| Critical / Error | `#EF4444` | same | `STATUS_DISABLED` |
| Normal | primary text | primary text | no extra hue for a healthy battery |
| Accent | `#3B82F6` | same | tray badge disc for a normal reading; OpenLogi brand accent |

The tray badge uses a saturated disc with white digits: accent blue for a
normal reading, green for charging/full, amber for low, red for critical/error;
a one-device-pixel ring separates it from the taskbar. Status is never conveyed
by colour alone: digits, the charging bolt, or the offline word are always
present.

## 4. Architecture and data flow

```text
HID++ (agent, unchanged)
        │
        ▼
ObservableState ── snapshot/observe ──────────────┐
        │                                        │
        ├── tray watcher (coalesced)             │
        │      ├─ Windows: render HICON, NIM_MODIFY
        │      └─ macOS: update status-item title
        │
        └── IPC observe ──► overlay process
                               │
                               ├─ BatteryWidgetModel (filter/order)
                               ├─ config.toml mtime watch (settings)
                               └─ BatteryWidgetView (GPUI)

GUI Settings ── atomic config.toml ──► agent reload ──► tray setting
                                      └──────────────► overlay mtime watch
```

- **No new IPC method and no wire-format change.** The overlay uses the existing
  `Observer::state`; the tray reads the in-process `ObservableState`. Widget
  settings are read from the same `config.toml` the GUI writes; position lives in
  an overlay-owned state file.
- **Config, not a new command channel.** The overlay reads `ConfigFile` (already
  available through `openlogi-core`'s default `fs` feature) and caches its
  modification time. A one-second background check is enough for a settings
  change to appear without restarting the agent or overlay. If the file is
  mid-write or invalid, keep the last-good settings and log.
- **One reading authority.** Move the desktop's private `charge_reading`,
  `battery_charging_no_reading`, and `battery_tone` into
  `openlogi-core::device` as public methods on `BatteryInfo`. The desktop, tray,
  and widget all consume them. This is the project's "single source of truth"
  rule, not a new architecture.
- **Tray lifecycle.** The observable is currently created inside
  `startup::bootstrap`, after `main` has already spawned the Windows tray. Move
  its construction just before the tray spawn (or hand it to the macOS tray
  through the armed handoff), then pass the same `Arc<ObservableState>` into the
  lifecycle. The tray thread uses `ObservableState::read` for a summary; a small
  watcher posts a coalesced update message on change. No device I/O moves.

## 5. Settings and configuration

Additive `AppSettings` fields, all serde-defaulted so old files keep loading:

| Field | Default | Scope | Meaning |
|---|---|---|---|
| `tray_battery` | `true` | macOS/Windows | Show the tray tooltip/menu readout and the badge icon |
| `battery_widget` | `true` | all platforms | Show the floating widget |
| `battery_widget_display_mode` | `Normal` | all platforms | `Normal` / `Topmost` / `Desktop` |
| `battery_widget_max_devices` | `3` | all platforms | 1–5 visible rows; values outside the range clamp |
| `battery_widget_show_offline` | `true` | all platforms | Include last-known offline devices |

- The values live in `[app_settings]` in `config.toml`; they do not require a
  schema-version bump because every field is additive and defaulted.
- Settings UI: a dedicated **Flow Indicator** tab after General (decided
  2026-10-27), not a group on the General page. It carries the tray switch
  (Windows today; macOS with its parity work), the widget switch, the
  show-offline switch, a 1–5 device-row radio group, and the Normal/Topmost
  radio group. Use the existing gpui-component settings vocabulary.
- New locale keys (same key/order in every catalog, checked by the parity test):
  `app.tray_battery`, `app.tray_battery_description`,
  `app.battery_widget`, `app.battery_widget_description`,
  `battery_widget.display_mode`, `battery_widget.display_mode_description`,
  `battery_widget.mode_normal`, `battery_widget.mode_topmost`,
  `battery_widget.mode_desktop`, `battery_widget.max_devices`,
  `battery_widget.max_devices_description`, `battery_widget.no_devices`.
  Reuse `device.battery`, `device.charging`, `device.offline`,
  `device.last_known_battery`, and the kind names where possible.
- Position: `battery-widget.toml` in the OpenLogi config directory, owned by the
  overlay. Fields: `x`, `y`, `display_id`; write atomically on drag end. The GUI
  and agent never write this file. On restore, clamp to the nearest work area;
  if the display is gone, fall back to the primary display.

**Rollback / kill switch**

- `tray_battery = false` restores the static brand icon, the old Show/Quit menu,
  and the old tooltip on the next agent config reload; no restart is required
  for the readout state itself.
- `battery_widget = false` closes the overlay window within the config watch
  interval; deleting `battery-widget.toml` only resets its position.
- A badge renderer failure falls back to the brand glyph; a widget window failure
  is logged and leaves the tray and GUI untouched.
- The feature adds no migration; reverting the commits removes the surfaces. No
  permission or elevation state has to be undone.

## 6. File-level change map

| Area | Files | Change | Tests |
|---|---|---|---|
| Shared semantics | `crates/openlogi-core/src/device.rs` | `BatteryReading`, `BatteryTone`, `reading()`, `tone()`, `bar_percentage()`, `charging_without_reading()` | exhaustive truth table for status/level/percentage; cold-start 0%; error |
| Desktop alignment | `crates/openlogi-desktop/src/ui/battery.rs` | delete private classifiers; use core methods; no visual change | existing battery tests + tone tests |
| Tray data | `crates/openlogi-agent/src/tray_battery.rs` (new) | `BatterySummary`, active/first-device selection, tooltip/menu text, pure badge renderer | all percentages/levels/tones, no devices, active-device fallback, tie-break |
| Tray Windows | `crates/openlogi-agent/src/tray_windows.rs`, `tray_battery/windows.rs` (new) | observable watcher, `HICON` creation/cache/destroy, `NIM_MODIFY`, tooltip/menu rows, `TaskbarCreated`, theme/DPI refresh | `cargo test -p openlogi-agent tray`; manual 100/150/200% DPI, light/dark taskbar |
| Tray macOS | `crates/openlogi-agent/src/tray.rs`, `status_item.rs` | status-item title, menu rows, main-queue update | build + manual macOS; not in first Windows PR |
| Tray wiring | `crates/openlogi-agent/src/main.rs`, `lifecycle.rs`, `startup.rs`, `server.rs` | create/pass `Arc<ObservableState>`; update tray setting on config reload | focused lifecycle/tray tests; no HID behavior change |
| Overlay model | `crates/openlogi-overlay/src/battery_widget.rs` (new) | snapshot filter/order/format, geometry, view, window lifecycle | pure model tests + `#[gpui::test]` render/update |
| Overlay placement | `crates/openlogi-overlay/src/battery_widget/{placement,windows,macos}.rs` (new) | work-area placement, z-order, drag, position persistence | pure geometry tests; Windows manual; macOS later |
| Overlay IPC | `crates/openlogi-overlay/src/ipc.rs`, `main.rs` | add `Observer::state` cell; present/close widget; settings mtime watcher | observer/lost state; window open/close tests |
| Settings | `crates/openlogi-core/src/config/settings.rs`, `crates/openlogi-desktop/src/state/settings.rs`, `windows/settings/general.rs` | fields, commits, UI group | config default/migration tests; settings UI test |
| Assets | `crates/openlogi-ui/src/action_icons.rs` and `action-icons/` | reuse `mouse.svg`, `keyboard.svg`, `battery-low.svg`, `bolt.svg`, `camera.svg`, `circle-dot.svg`; add `headset.svg`/`gamepad-2.svg` only if needed | embedded-asset test |
| i18n | `crates/openlogi-ui/locales/*.toml` | new keys in every catalog, same order | `cargo test -p openlogi-ui locale` |
| Docs | `docs/CONFIGURATION.md`, this plan | new settings, user-facing description | prose review |

No `crates/openlogi-ipc/src/ipc.rs` change is expected. If a future iteration
adds a push setting channel, it must follow `crates/openlogi-ipc/AGENTS.md`:
append-only method/enum, `PROTOCOL_VERSION` discipline, and a regenerated
`wire_format` golden.

## 7. Delivery plan

Recommended order (why: prove the shared reading rules and tray data path before
the larger GPUI window; the tray pieces are independently shippable):

1. **M0 — Shared semantics.** Move `charge_reading`, `battery_tone`, and the
   cold-start rule into `openlogi-core`; make the desktop use them. No behavior
   change. Exit: existing GUI battery tests plus new truth-table tests pass.
2. **M1a — Tray data, tooltip, menu.** Add `tray_battery` setting; pass the
   observable to the tray; add tooltip and device rows under the setting. No
   icon change yet. Exit: menu/tooltip show the same readings and status words
   as the GUI; off restores the current tray exactly.
3. **M1b — Tray badge icon (Windows).** Pure RGBA renderer + `HICON` adapter;
   controlled by `tray_battery`; fallback to the brand glyph. Exit: 16/20/24 px
   screenshots on light/dark taskbars at 100/150/200% DPI; Explorer restart
   re-adds the current badge; no GDI/icon leaks.
4. **M2 — Widget MVP.** Overlay window with `Observer::state`, fixed bottom-right
   placement, all device rows including offline last-known, default max 3,
   Normal mode; config mtime visibility switch. No drag/mode/scroll yet. Exit:
   mock-agent screenshots with 1/3/5 devices, charging, low, no-reading,
   offline; no focus steal; window disappears when disabled or when no device
   has ever had a reading.
5. **M3 — Widget productization.** Settings UI, drag, position persistence,
   display modes, max-device select, scroll, OS light/dark palettes, scale
   handling, optional right-click "Show OpenLogi". Exit: manual matrix across
   monitors/DPI/modes; settings apply live; position survives restart; no
   Alt-Tab entry.
6. **M4 — macOS tray title and widget parity; Linux fallback.** Separate PRs;
   macOS `NSStatusItem` title and native window level/drag; Linux layer-shell
   fallback (no drag). Exit: explicit macOS/Linux verification or a clear
   untested report.

If the visual widget is the priority, M2 can run immediately after M0 and M1a
can follow; M1b does not block the widget.

## 8. Verification

**Automated**

```sh
cargo test -p openlogi-core battery
cargo test -p openlogi-desktop battery
cargo test -p openlogi-agent tray
cargo test -p openlogi-overlay battery_widget
cargo test -p openlogi-ui locale
cargo fmt --check
cargo clippy -p openlogi-core -p openlogi-desktop -p openlogi-agent -p openlogi-overlay --all-targets -- -D warnings
```

No wire-format test is required while the IPC contract is untouched. Any change
to `AgentSnapshot`, `Observation`, or the tarpc trait changes that answer.

**Mock-agent workflow**

```sh
cargo run -p openlogi-agent --bin openlogi-agent-mock
# In the mock console:
battery 1 87 charging
battery 1 15
offline 1
battery 1 60
```

Run the overlay against the mock profile and inspect the widget; run the desktop
with `OPENLOGI_DEV_AGENT=0` for the settings page. Confirm the tray by running
the real agent against the mock/dev profile where possible.

**Manual matrix (Windows first)**

- tray: 100/150/200% DPI; light/dark taskbar; 0%, 5%, 20%, 21%, 100%; charging;
  no reading; no devices; Explorer restart; menu with 1/3/6 devices.
- widget: 1/3/5 devices; charging/low/no-reading/offline; drag; Normal/Topmost/
  Desktop; multi-monitor; display unplugged; restart position restore; no
  Alt-Tab/taskbar entry; clicking does not activate the GUI; fullscreen game in
  Normal mode.
- settings: toggle both surfaces off restores the current UI; language switch
  updates labels; config hand-edit clamps max devices and falls back on invalid
  values.

macOS and Linux are not implied by Windows-green. The final report must name
which platform checks were not run. Screenshots for visual approval are captured
from the real window; a headless render is not visual proof.

## 9. Risks

| Risk | Mitigation |
|---|---|
| 16 px tray digits unreadable / poor alpha | Pure renderer with supersampling; screenshots at all sizes; fallback to brand glyph; alternative battery-glyph design |
| `HICON`/GDI leak on every update | `TrayIconState` owns the handle; destroy on replace/drop; update only when the badge/tooltip changes |
| GPUI z-order/drag/focus differences | M2 fixed, non-activating window first; platform seam spike before productization; default `Normal`, no focus steal |
| Persistent widget consumes resources | Render only on agent state changes; no polling loop beyond the 1 s config mtime stat; close when disabled/empty |
| Config mtime path races with atomic saves | Read `ConfigFile` atomically; on parse failure keep last-good; never write config from the overlay |
| Duplicate thresholds between GUI/tray/widget | M0 single source in core; ast-grep-style guard if a second threshold appears |
| Brand/upstream rejection of a replacement tray icon | Default-on product decision; keep a switch and a brand-glyph variant; design issue with mockups |
| 24-locale catalog churn | Reuse existing device keys; add only settings/empty-state keys; parity test enforces order |
| macOS/Linux parity | Windows-first milestones; macOS/Linux PRs separate; explicit untested gaps |
| License contamination | Reimplement in Rust; no FerrisPulse GPL assets; vigil-watt MIT attribution only if any code is copied |

## 10. Decisions and remaining questions

**Decided 2026-10-27**

1. **Tray default and device.** Tray readout is on by default. The badge uses the
   device currently in use (`selected_device`) when it can be matched, otherwise
   the first online device with a reading. It is not a lowest-battery badge.
2. **Widget default and placement.** Widget is on by default, bottom-right, at
   most 3 rows, `Normal` mode.
3. **Offline rows.** Show last-known offline devices with an `Offline` marker;
   use a session cache when the inventory drops the reading.
4. **Settings delivery.** The overlay reads `config.toml` (mtime watch); no new
   IPC observation in v1.

**Still open**

5. **Active-device matching.** If mapping `selected_device` to the inventory
   needs config-key reconstruction, v1 may use "first online device with a
   reading" and add active-device matching as a follow-up. Confirm whether the
   first PR should include the mapping.
6. **Platform order.** Windows first (source UI and target machine), macOS
   second, Linux fallback last.
7. **Tray icon style.** The decided default is the vigil-watt-style badge with
   digits; a brand-mark-with-tone-ring variant remains a design option if the
   default-on badge is considered too much brand drift.
8. **Transport tags.** Omit in v1; a later Bolt/Unifying/Bluetooth/USB tag can
   reuse `DeviceModelInfo`/`ReceiverInfo` without device-support work.
9. **Theme.** Solid translucent card following OS appearance in v1; true
   acrylic/blur later.

## 11. License

- **vigil-watt:** MIT. Its layout/interaction can be reimplemented; if any code
  or asset is copied verbatim, retain the MIT copyright/permission notice. The
  plan assumes a fresh Rust implementation, so no C# is linked.
- **FerrisPulse:** GPLv3. Not used by this plan. Its digit arrays/bitmaps must
  not be copied into OpenLogi without the copyright holder's permission.
- **OpenLogi:** MIT/Apache-2.0. The new presentation code follows the workspace
  license; no GPL-only asset enters the tree.
- No new system permission, elevation, network request, or telemetry is needed.

## 12. Linux verification log (2026-10-28)

Environment: Ubuntu 26.04 under WSL2. Wayland was tested in a nested labwc
0.9.3 inside WSLg (`WLR_BACKENDS=wayland WLR_RENDERER=pixman`; the VM has no
`/dev/dri`, so WSLg runs in copy mode and only software surfaces are visible
in the host window). X11 was tested over xrdp into XFCE (`DISPLAY=:10`,
xrdp on 3390; `PrivateTmp` gives the Xorg socket a writable `/tmp`). Both
runs used the mock agent and the overlay; screenshots came from inside the
compositor (`grim`) and the X11 root (`scrot`).

Results:

- Rendering: 3 and 4 rows, percentages, bars, charging bolt, 100% green,
  offline rows with their last reading and the `Offline` tag, and the
  low-battery notice. Mock console changes reach the card live.
  `flow_indicator = false` closes the window; restoring it reopens.
- Empty state: every device offline with `flow_indicator_show_offline =
  false` hides the window.
- Normal vs Topmost: X11 Normal is a managed, coverable window and Topmost
  is override-redirect above a terminal. Wayland layer-shell Normal (bottom
  layer) is covered by a terminal; Topmost (top layer) stays above it.
- Drag (before the fix below): dragging the card body moves nothing on
  either backend. Before the decoration change an X11/labwc title-bar drag
  moved the window; with client-side decorations there is no title bar, so
  the card could not be moved on Linux at all.
- Position (before the fix below): an X11 title-bar move updates
  `flow-indicator.toml`, but a restart opens at the WM's placement and the
  file is overwritten — restore does not round-trip. A layer surface has no
  position; its anchor decides.

### X11 drag and position restore (2026-10-28, second round)

The X11 half of gaps 2 and 3 is fixed by an overlay-owned second X11
connection (`crates/openlogi-overlay/src/platform/x11.rs`). GPUI's move entry
point is unusable here: `Window::start_window_move` sends
`_NET_WM_MOVERESIZE` with button 0, and xfwm4 (4.20, read from
`src/netwm.c:clientNetMoveResize`) drives its move loop from that button
number, so the drag never starts. `on_hit_test_window_control` stays a no-op
on both X11 and Wayland. The overlay therefore moves the window itself with
`ConfigureWindow` on every pointer motion, which works for both kinds: a
managed window becomes a `ConfigureRequest` the WM applies (xfwm4 honors it
for a client's own window) and an override-redirect `PopUp` simply moves.
The same request performs position restore after open, retried on the
one-second tick because the first request can reach the WM before it manages
the freshly created window; saving is paused while a placement is pending so
the WM's own placement cannot overwrite the saved position.

Verified live on xrdp/XFCE (1920×1080, scale 1):

- Normal: first run opens at the requested bottom-right (1596, 885) instead
  of the WM's centred placement; a pointer drag of (+150, +100) lands exactly
  (+150, +100); `flow-indicator.toml` follows; a restart restores the dragged
  position.
- Topmost: the window is override-redirect; the same drag and restore both
  round-trip.
- Taskbar: the managed card is a normal task-list window titled
  `OpenLogi - Flow Indicator`, so it can be minimized, moved between
  workspaces and closed like any other window. Closing it retires the card
  until a settings change: X11 destroys the window without the overlay's
  close callback ever running (GPUI keeps the removed window in its map), so
  the overlay notices the closed window on its one-second tick by asking the
  X11 connection (`BadWindow`) and removes the stale window itself. A card in
  Topmost mode is override-redirect and therefore never gets a task-list
  button.
- Wayland (layer shell) is unchanged by this: the seam only matches an Xcb
  raw handle, and the Wayland path still calls `start_window_move`, which is
  a no-op on a layer surface. Wayland drag interaction remains unverified
  (needs a real pointer in the nested compositor).

Gaps found, and their status after the 2026-10-28 fix:

1. **Server-side title bars** (KWin/labwc SSD, X11 WM frames) — fixed by
   requesting client-side decorations for the card and the other overlay
   windows; X11 applies `_MOTIF_WM_HINTS`, Wayland sets xdg-decoration.
2. **No card drag on Linux** — fixed on X11 (both display modes); Wayland
   layer surfaces are anchored by the protocol and stay fixed, and an
   ordinary Wayland toplevel gets the compositor's `_move`. Wayland
   interaction unverified.
3. **Position memory** — fixed on X11 (saved position survives a restart in
   both modes). Layer surfaces have no position to save; an ordinary Wayland
   toplevel is placed by the compositor.
4. **Topmost** — fixed on Wayland with `Layer::Top`; X11 already worked
   through `WindowKind::PopUp` (override-redirect). A compositor without
   wlr-layer-shell falls back to an ordinary window, where topmost remains
   unimplemented.

### Card clipped at the window's bottom edge (2026-10-28, third round)

`card_size` assumed a 22 px row line (the icon lane) and no border, but
GPUI's default text line height is 1.618x the font size: the 16 px reading
lays out in a 26 px line. Three rows are 185 px tall while the window was
171 px, so the window edge clipped the last row's bar and the bottom padding.
This was not XFCE-specific; it applied wherever the card was drawn.

The card's arithmetic now adds the 26 px line box and both 1 px borders, and
`row_view` pins the line box with `line_height`/`height` so the layout and
`card_size` cannot drift. A `gpui::test` renders three rows and asserts the
laid-out card height equals `card_size`; xrdp/XFCE screenshots before and
after the fix show 1 px versus 14 px of card below the last bar (window
300x171 -> 300x185).

### The tray on Linux (2026-10-28, fourth round)

The plan's last platform piece is the tray itself. Linux now hosts the same
readout as a StatusNotifierItem (`crates/openlogi-agent/src/tray_linux.rs`,
via `ksni`): one implementation for X11 and Wayland, because the protocol is
D-Bus-only, and the multi-size `IconPixmap` bitmaps the badge already
produces for Windows are what every SNI host renders. A desktop without an
SNI host is a soft failure — the service waits and registers when a watcher
appears.

Verified live in the xrdp/XFCE session. XFCE 4.20's `systray` plugin ships an
SNI watcher (the `wrapper-2.0` helper owns `org.kde.StatusNotifierWatcher`),
so this desktop needs no indicator bridge:

- Registration and rendering: the real agent (empty inventory) registers and
  shows the brand icon; a throwaway example feeding one synthetic 64% device
  renders the blue circle badge in the panel, through the same pixmap path
  Windows uses.
- Left click delivers `Activate`. The bus shows the panel's SNI host calling
  `Activate` on the item, and the handler spawns the GUI found next to the
  tray binary — the window mapped on the X11 desktop. An earlier "clicks do
  nothing" reading was two artifacts stacked: the icon's hit area is the
  roughly 22 px systray slot (a click a few pixels right lands on empty
  panel and opens the panel's own menu), and a harness without a tracing
  subscriber hides `open_gui`'s warnings.
- Right click delivers the menu. The panel renders the dbusmenu with the
  disabled device row, Show Main Window, and Quit OpenLogi
  (`AboutToShowGroup`/`EventGroup` on the wire), and activating an entry
  runs its callback.
- Menu Quit ends the agent gracefully — after a fix. The shared
  `request_tray_quit` blocked its caller until the core finished teardown,
  which is a panic inside the ksni runtime ("Cannot block the current
  thread from within a runtime"); the request itself was already delivered,
  so the shutdown completed and only the panic line betrayed the wrong
  shape. The macOS/Windows form keeps blocking (their callbacks own plain
  OS threads); the Linux form hands the request over and returns, leaving
  the lifecycle's exit path in charge. Re-verified live: clean exit, no
  panic, and the item leaves the watcher.

Hosts without an environment here (KDE, GNOME + its AppIndicator extension,
waybar) remain unverified; CONFIGURATION.md now states the host requirement
instead of claiming Linux has no tray.

### Percentage colours, product names, and the wide Linux face (2026-10-29)

- **Three-stage percentage colours** land on every surface: the GUI's battery
  glyph and percentage, the keyboard and light brightness percentages, the
  Flow Indicator digits and bars, and the tray badge. The band is 70%+ green,
  40–69% amber, below 40% red; charging and charge-complete stay green, a fault
  stays red, and a level-only reading keeps its firmware tone. Thresholds live
  in `openlogi_core::device` (`BatteryStage`, `BatteryInfo::color`). Verified
  live in the xrdp/XFCE session by sampling the SNI pixmaps the agent serves
  (55% amber, 25% red, 45% charging green bolt) and by GUI/overlay screenshots
  (79% green digits, card 55% amber / 71% green).
- **Product names.** The tray rows and the Flow Indicator read the names the
  settings app persists (`MX Master 3S`, `MX Keys`, `MX Vertical`) instead of
  the firmware codenames; a user-assigned custom name outranks them, and a
  device with no entry (or a standalone light) keeps its codename. The
  resolution is `Config::paired_display_name`, shared by both surfaces; the GUI
  now reloads the agent after an identity or custom-name save so the rows
  follow without a restart. Verified on the D-Bus menu
  (`MX Master 3S  77%`, `MX Keys  100%`, `MX Vertical  55%`).
- **The demo tree now hosts the overlay.** The debug agent starts the overlay
  supervisor at the same arming point as the production lifecycle (and evicts
  the helper on a tray Quit). Before this, the demo path never spawned an
  overlay, so the Flow Indicator could not appear in the debug tree whatever
  the setting said. Verified live: the agent logs `helper started`, the card
  renders with the demo devices, and switching `flow_indicator` off/on closes
  and reopens it within the one-second config poll.
- **Names follow the surfaces.** The low-battery notice uses the persisted
  product name (custom name first, then the settings app's name, then the
  firmware codename) in both the production orchestrator and the demo mock.
  The canonical demo's never-online mouse now carries a real product codename
  (`MX Anywhere 3S`) instead of a synthetic one, so its offline row is named
  like the rest.
- **XFCE keeps the plain badge.** `tray_badge_style = "wide"` is confined to
  desktops that can show it: when the session is XFCE (the environment, or an
  `xfce4-panel` host process when the agent was started outside the desktop's
  environment) the tray falls back to the circle badge, which is what XFCE's
  icon-slot sizing can display.
- **Wide Linux face** (`tray_badge_style = "wide"`, config file only): one
  SNI pixmap carries the name, a progress bar, and the percentage
  (142x22 px at the panel slot, 284x44 for 2x). The face itself renders as
  designed (`MX MASTER 3S` + bar + `64%`), but XFCE 4.20's systray scales any
  tray pixmap into the panel's icon slot, so the wide face becomes a ~33x6
  stripe and is unreadable there. The style therefore stays off by default;
  hosts that keep a wide item's aspect ratio at a usable size (KDE, waybar)
  remain untested.

## Appendix: vigil-watt reference

- Tray: lowest online non-system device; 32×32 GDI+ disc; white percentage or
  charging bolt; tooltip; right-click menu with disabled device rows; refresh
  coalesced and driven by device events
  (`src/VigilWatt.App/Services/TrayService.cs` lines 178–311).
- Widget: borderless card; 16/14 padding; 22 px icon; 13 px name; 10 px source
  tag; charging mark; 16 px semi-bold percentage; 6 px bar; 1–5 visible rows;
  scroll; drag; saved position; Topmost/Normal/Desktop modes; OS light/dark
  skins (`WidgetWindow.xaml`, `WidgetWindow.xaml.cs` lines 165–516).
- Differences for OpenLogi: the tray follows the active/first device rather than
  the lowest battery; shared OpenLogi reading thresholds/colors instead of
  vigil-watt's 20/50/80 palette; offline rows are kept with last-known values;
  no taskbar pill; no second low-battery balloon; settings in the settings app
  rather than a custom WinUI page.
